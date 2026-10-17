use anyhow::{Result, bail};
use reqwest::{Client, Url};
use serde_json::{Value, json};
#[cfg(test)]
use std::path::Path;
use std::{path::PathBuf, time::Duration};

mod cookies;
#[cfg(unix)]
mod fs_safe;
#[cfg(not(unix))]
mod fs_safe {
    use anyhow::{Result, bail};
    use std::{fs::File, path::Path};
    pub(super) fn private_read(_: &Path, _: usize) -> Result<Vec<u8>> {
        bail!("developer_platform_unsupported")
    }
    pub(super) fn private_lock(_: &Path, _: &str) -> Result<File> {
        bail!("developer_platform_unsupported")
    }
    pub(super) fn atomic_write(_: &Path, _: &[u8]) -> Result<()> {
        bail!("developer_platform_unsupported")
    }
}
mod groups;
mod shared;
mod state;
pub(crate) use cookies::PortalCookie;

#[derive(Clone)]
pub struct DeveloperPortal {
    client: Client,
    root: PathBuf,
    source: PathBuf,
    endpoint: Url,
    #[cfg(test)]
    fail_receipt: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone, Copy)]
enum Endpoint {
    Teams,
    Groups,
    Apps,
    Detail,
    Create,
    Associate,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Teams => "account/listTeams.action",
            Self::Groups => "account/ios/identifiers/listApplicationGroups.action",
            Self::Apps => "account/ios/identifiers/listAppIds.action",
            Self::Detail => "account/ios/identifiers/getAppIdDetail.action",
            Self::Create => "account/ios/identifiers/addApplicationGroup.action",
            Self::Associate => "account/ios/identifiers/assignApplicationGroupToAppId.action",
        }
    }
}

pub(crate) fn validate_team(value: &str) -> Result<()> {
    if value.len() != 10
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        bail!("developer_team_invalid");
    }
    Ok(())
}

impl DeveloperPortal {
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var("ASC_DEVELOPER_APP_GROUPS_ENABLED") {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Ok(value) if value == "false" => Ok(None),
            Ok(value) if value == "true" => Self::new_default().map(Some),
            _ => bail!("developer_configuration_invalid"),
        }
    }

    pub fn new_default() -> Result<Self> {
        if !cfg!(unix) {
            bail!("developer_platform_unsupported");
        }
        let root = match std::env::var_os("ASC_DEVELOPER_STATE_ROOT") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            Some(_) => bail!("developer_configuration_invalid"),
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .ok_or_else(|| anyhow::anyhow!("developer_configuration_invalid"))?,
            )
            .join("Library/Application Support/ASC MCP/Developer Portal"),
        };
        let source = match std::env::var_os("ASC_DEVELOPER_COOKIE_PATH") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            Some(_) => bail!("developer_configuration_invalid"),
            None => root.join("developer-session.cookie"),
        };
        Self::new(Url::parse("https://developer.apple.com/")?, root, source)
    }

    fn new(endpoint: Url, root: PathBuf, source: PathBuf) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| anyhow::anyhow!("developer_configuration_invalid"))?;
        Ok(Self {
            client,
            endpoint,
            root,
            source,
            #[cfg(test)]
            fail_receipt: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    #[cfg(test)]
    fn fail_next_receipt_write(&self) {
        self.fail_receipt
            .store(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn for_test(endpoint: &str, root: &Path, source: &Path) -> Result<Self> {
        let endpoint = Url::parse(endpoint)?;
        if !matches!(
            endpoint.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        ) || endpoint.scheme() != "http"
        {
            bail!("developer_test_origin_invalid");
        }
        Self::new(endpoint, root.into(), source.into())
    }

    async fn request(
        &self,
        transaction: &mut state::Transaction,
        endpoint: Endpoint,
        fields: Vec<(String, String)>,
    ) -> Result<Value> {
        self.request_checked(transaction, endpoint, fields, &mut false)
            .await
    }

    async fn request_checked(
        &self,
        transaction: &mut state::Transaction,
        endpoint: Endpoint,
        fields: Vec<(String, String)>,
        rejected: &mut bool,
    ) -> Result<Value> {
        let path = format!("/services-account/QH65B2/{}", endpoint.path());
        let effective = Url::parse(&format!("https://developer.apple.com{path}"))
            .map_err(|_| anyhow::anyhow!("developer_endpoint_invalid"))?;
        if !transaction.generation.is_empty() {
            transaction.ensure_generation()?;
        }
        let mut cookie = reqwest::header::HeaderValue::from_str(&transaction.header(&effective)?)
            .map_err(|_| anyhow::anyhow!("developer_cookie_invalid"))?;
        cookie.set_sensitive(true);
        let url = self
            .endpoint
            .join(&path)
            .map_err(|_| anyhow::anyhow!("developer_endpoint_invalid"))?;
        let mut request = self
            .client
            .post(url)
            .header(reqwest::header::COOKIE, cookie)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some((csrf, stamp)) = &transaction.csrf {
            request = request.header("csrf", csrf).header("csrf_ts", stamp);
        }
        let mut response = request
            .form(&fields)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("developer_transport_failed"))?;
        let headers = response.headers().clone();
        transaction.rotate_response(&headers, &effective)?;
        transaction.remember_secrets(
            ["csrf", "csrf_ts"]
                .into_iter()
                .flat_map(|name| headers.get_all(name).iter())
                .filter_map(|value| value.to_str().ok().map(str::to_owned))
                .collect::<Vec<_>>(),
        )?;
        if !response.status().is_success() {
            bail!("developer_http_failed");
        }
        let media = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        if !media.is_some_and(|media| media.eq_ignore_ascii_case("application/json")) {
            bail!("developer_response_media_invalid");
        }
        if headers.get_all("csrf").iter().count() > 1
            || headers.get_all("csrf_ts").iter().count() > 1
        {
            bail!("developer_csrf_invalid");
        }
        let csrf = headers.get("csrf").and_then(|value| value.to_str().ok());
        let stamp = headers.get("csrf_ts").and_then(|value| value.to_str().ok());
        match (csrf, stamp) {
            (Some(csrf), Some(stamp))
                if [csrf, stamp].iter().all(|value| {
                    !value.is_empty()
                        && value.len() <= 4096
                        && value.bytes().all(|byte| (33..127).contains(&byte))
                }) =>
            {
                if matches!(endpoint, Endpoint::Groups) {
                    transaction.csrf = Some((csrf.into(), stamp.into()));
                }
            }
            (None, None) => (),
            _ => bail!("developer_csrf_invalid"),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("developer_transport_failed"))?
        {
            if bytes.len() + chunk.len() > 8 << 20 {
                bail!("developer_response_too_large");
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("developer_response_invalid"))?;
        transaction.reject_secret_echo(&body)?;
        match body["resultCode"].as_i64() {
            Some(0) => (),
            Some(_) => {
                *rejected = true;
                bail!("developer_rejected");
            }
            None => bail!("developer_response_invalid"),
        }
        Ok(body)
    }

    async fn verify_team(&self, transaction: &mut state::Transaction, team: &str) -> Result<()> {
        validate_team(team)?;
        if let state::Authentication::Dedicated(source) = &transaction.auth
            && source.team_id != team
        {
            bail!("developer_team_mismatch");
        }
        let identities = self.teams(transaction).await?;
        if !identities.contains(team) {
            bail!("developer_team_unavailable");
        }
        transaction.team_id = Some(team.into());
        Ok(())
    }

    async fn teams(
        &self,
        transaction: &mut state::Transaction,
    ) -> Result<std::collections::BTreeSet<String>> {
        let body = self.request(transaction, Endpoint::Teams, vec![]).await?;
        let teams = body["teams"]
            .as_array()
            .filter(|teams| teams.len() <= 1000)
            .ok_or_else(|| anyhow::anyhow!("developer_teams_invalid"))?;
        let mut identities = std::collections::BTreeSet::new();
        for item in teams {
            let identity = item["teamId"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("developer_teams_invalid"))?;
            validate_team(identity)?;
            if !identities.insert(identity.to_owned()) {
                bail!("developer_teams_invalid");
            }
        }
        Ok(identities)
    }

    pub(crate) async fn publish_login(
        &self,
        team_id: &str,
        cookies: Vec<PortalCookie>,
    ) -> Result<PathBuf> {
        validate_team(team_id)?;
        cookies::validate(&cookies)?;
        let source = state::Source {
            schema: 1,
            login_id: uuid::Uuid::new_v4().to_string(),
            team_id: team_id.into(),
            cookies: cookies.clone(),
        };
        let lock = state::acquire(&self.root)?;
        let secrets = cookies.iter().map(|cookie| cookie.value.clone()).collect();
        let mut transaction = state::Transaction {
            _lock: lock,
            root: self.root.clone(),
            source_path: self.source.clone(),
            team_id: Some(team_id.into()),
            auth: state::Authentication::Dedicated(source),
            generation: String::new(),
            cookies,
            csrf: None,
            secrets,
        };
        tokio::time::timeout(
            Duration::from_secs(120),
            self.verify_team(&mut transaction, team_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("developer_operation_timeout"))??;
        cookies::header(
            &transaction.cookies,
            &Url::parse(
                "https://developer.apple.com/services-account/QH65B2/account/listTeams.action",
            )?,
        )?;
        let state::Authentication::Dedicated(mut source) = transaction.auth else {
            bail!("developer_session_invalid");
        };
        source.cookies = transaction.cookies.clone();
        let bytes =
            serde_json::to_vec(&source).map_err(|_| anyhow::anyhow!("developer_state_invalid"))?;
        if bytes.len() > 1 << 20 {
            bail!("developer_state_too_large");
        }
        fs_safe::atomic_write(&self.source, &bytes)?;
        if fs_safe::private_read(&self.source, 1 << 20)? != bytes {
            bail!("developer_state_unverified");
        }
        Ok(self.source.clone())
    }

    pub fn tools(&self) -> Vec<Value> {
        let string = |pattern: &str| json!({"type":"string","pattern":pattern});
        let team = string("^[A-Z0-9]{10}$");
        let identity = json!({"type":"string","minLength":1,"maxLength":255});
        let operation = json!({"type":"string","format":"uuid"});
        let descriptors = [
            (
                "get_developer_connection_status",
                json!({}),
                vec![],
                true,
                false,
            ),
            (
                "list_app_groups",
                json!({"team_id":team}),
                vec!["team_id"],
                true,
                false,
            ),
            (
                "get_bundle_id_app_groups",
                json!({"team_id":team,"bundle_id":identity}),
                vec!["team_id", "bundle_id"],
                true,
                false,
            ),
            (
                "create_app_group",
                json!({"team_id":team,"identifier":identity,"name":string("^[A-Za-z0-9][A-Za-z0-9 _-]{0,99}$"),"dry_run":{"type":"boolean","default":true},"operation_id":operation,"confirmation_token":string("^[a-fA-F0-9]{64}$")}),
                vec!["team_id", "identifier", "name"],
                false,
                false,
            ),
            (
                "associate_app_group",
                json!({"team_id":team,"identifier":identity,"bundle_id":identity,"dry_run":{"type":"boolean","default":true},"operation_id":operation,"confirmation_token":string("^[a-fA-F0-9]{64}$")}),
                vec!["team_id", "identifier", "bundle_id"],
                false,
                true,
            ),
            (
                "get_app_group_write_status",
                json!({"operation_id":operation}),
                vec!["operation_id"],
                true,
                false,
            ),
        ];
        descriptors.into_iter().map(|(name,properties,required,read,destructive)| json!({"name":name,
            "description":"Experimental undocumented Developer Portal operation. Reuses eligible credentials from the configured reviewer when no dedicated Developer session exists; live Developer access and explicit Team verification remain required. Writes default to a stored dry-run plan; execute only after explicit operator approval. Unknown delivery must never be retried.",
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":read,"destructiveHint":destructive,"idempotentHint":read,"openWorldHint":true}})).collect()
    }

    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        self.call_with_reviewer(name, args, None).await
    }

    pub(crate) async fn call_with_reviewer(
        &self,
        name: &str,
        args: &Value,
        reviewer: Option<&crate::reviewer::Reviewer>,
    ) -> Result<Value> {
        tokio::time::timeout(
            Duration::from_secs(120),
            self.call_inner(name, args, reviewer),
        )
        .await
        .map_err(|_| anyhow::anyhow!("developer_operation_timeout"))?
    }

    async fn call_inner(
        &self,
        name: &str,
        args: &Value,
        reviewer: Option<&crate::reviewer::Reviewer>,
    ) -> Result<Value> {
        let started = std::time::Instant::now();
        let write = match name {
            "create_app_group" => Some(groups::write_args(args, groups::Kind::Create)?),
            "associate_app_group" => Some(groups::write_args(args, groups::Kind::Associate)?),
            "list_app_groups" => {
                groups::team_args(args, false)?;
                None
            }
            "get_bundle_id_app_groups" => {
                groups::team_args(args, true)?;
                None
            }
            "get_app_group_write_status" => {
                groups::status_args(args, true)?;
                None
            }
            "get_developer_connection_status" => {
                groups::status_args(args, false)?;
                None
            }
            _ => bail!("developer_tool_unknown"),
        };
        let loaded = state::Transaction::load_with_shared(
            &self.root,
            &self.source,
            reviewer.map(crate::reviewer::Reviewer::shared_session),
        );
        if name == "get_developer_connection_status" {
            return match loaded {
                Ok(mut transaction) => {
                    let team = transaction.team_id.clone();
                    let verified = match &team {
                        Some(team) => self.verify_team(&mut transaction, team).await.map(|()| 1),
                        None => self.teams(&mut transaction).await.map(|teams| teams.len()),
                    };
                    match verified {
                        Ok(count) => {
                            transaction.persist_overlay()?;
                            let mut status = json!({"configured":true,"status":"connected","auth_source":transaction.auth_source(),"experimental":true});
                            if let Some(team) = team {
                                status["team_id"] = json!(team);
                            } else {
                                status["team_count"] = json!(count);
                                status["team_required_for_operations"] = json!(true);
                            }
                            Ok(status)
                        }
                        Err(_) => Ok(
                            json!({"configured":true,"status":"access_unverified","experimental":true}),
                        ),
                    }
                }
                Err(error) if error.to_string() == "developer_busy" => Err(error),
                Err(_) => Ok(
                    json!({"configured":true,"status":"authentication_required","experimental":true}),
                ),
            };
        }
        let mut transaction = loaded?;
        let mut receipts = state::Receipts::load(self)?;
        let team = if let Some(write) = &write {
            write.plan.team.clone()
        } else if name == "get_app_group_write_status" {
            let id = groups::status_args(args, true)?.unwrap_or_default();
            receipts
                .records
                .iter()
                .find(|record| record.operation_id == id)
                .ok_or_else(|| anyhow::anyhow!("developer_operation_unknown"))?
                .plan
                .team
                .clone()
        } else {
            groups::team_args(args, name == "get_bundle_id_app_groups")?
                .0
                .into()
        };
        self.verify_team(&mut transaction, &team).await?;
        if let Some(write) = write {
            receipts.check_conflict(&write.plan)?;
            let confirmed = if write.dry_run {
                None
            } else {
                Some(receipts.confirmed(&write, &transaction.generation)?)
            };
            if let Some(index) = confirmed {
                let preparation = async {
                    let (plan, no_op) = self.resolve_plan(&mut transaction, &write.plan).await?;
                    transaction.persist_overlay()?;
                    if plan != receipts.records[index].plan || no_op.is_some() {
                        bail!("developer_plan_changed");
                    }
                    self.prepare_mutation(&mut transaction, &plan, started)
                        .await?;
                    Ok::<_, anyhow::Error>(plan)
                }
                .await;
                let outcome = match preparation {
                    Ok(plan) => {
                        receipts.records[index].state = state::Delivery::Attempting;
                        receipts.records[index].observation = "attempting".into();
                        receipts.save(self)?;
                        self.mutate(&mut transaction, &plan, started).await
                    }
                    Err(error) => groups::MutationOutcome::NotDispatched(error),
                };
                let (delivery, observation, local_error) = match outcome {
                    groups::MutationOutcome::Finished(Ok(())) => {
                        (state::Delivery::Verified, "verified", None)
                    }
                    groups::MutationOutcome::AppleRejected => {
                        (state::Delivery::Rejected, "apple_rejected", None)
                    }
                    groups::MutationOutcome::Finished(Err(_)) => {
                        (state::Delivery::Unknown, "delivery_unknown", None)
                    }
                    groups::MutationOutcome::NotDispatched(error) => {
                        (state::Delivery::Rejected, "not_dispatched", Some(error))
                    }
                };
                receipts.records[index].state = delivery;
                receipts.records[index].observation = observation.into();
                if receipts.save(self).is_err() {
                    receipts.records[index].state = state::Delivery::Unknown;
                    receipts.records[index].observation = "delivery_unknown".into();
                    let _ = receipts.save(self);
                    bail!("developer_delivery_unknown_receipt_unverified");
                }
                if transaction.persist_overlay().is_err()
                    && receipts.records[index].state == state::Delivery::Verified
                {
                    receipts.records[index].state = state::Delivery::Unknown;
                    receipts.records[index].observation = "delivery_unknown".into();
                    receipts.save(self)?;
                }
                if let Some(error) = local_error {
                    return Err(error);
                }
                return Ok(receipts.records[index].summary());
            }
            let (plan, no_op) = self.resolve_plan(&mut transaction, &write.plan).await?;
            transaction.persist_overlay()?;
            if let Some(no_op) = no_op {
                return Ok(no_op);
            }
            return receipts.prepare(self, plan);
        }
        let result = match name {
            "list_app_groups" => {
                json!({"team_id":team,"groups":self.groups(&mut transaction).await?})
            }
            "get_bundle_id_app_groups" => {
                self.read_groups_for_bundle(
                    &mut transaction,
                    groups::team_args(args, true)?.1.unwrap_or_default(),
                )
                .await?
            }
            "get_app_group_write_status" => {
                let id = groups::status_args(args, true)?.unwrap_or_default();
                let record = receipts
                    .records
                    .iter_mut()
                    .find(|record| record.operation_id == id)
                    .ok_or_else(|| anyhow::anyhow!("developer_operation_unknown"))?;
                if record.state == state::Delivery::Unknown {
                    record.observation = match self.observe(&mut transaction, &record.plan).await {
                        Ok(true) => "desired_state_present",
                        Ok(false) => "desired_state_absent",
                        Err(_) => "observation_unavailable",
                    }
                    .into();
                }
                let summary = record.summary();
                receipts.save(self)?;
                summary
            }
            _ => bail!("developer_tool_unknown"),
        };
        transaction.persist_overlay()?;
        Ok(result)
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    #[test]
    fn developer_optional_support_is_explicitly_unavailable() {
        let error = super::DeveloperPortal::new_default()
            .err()
            .expect("non-Unix Developer setup must fail");
        assert_eq!(error.to_string(), "developer_platform_unsupported");
    }
}
