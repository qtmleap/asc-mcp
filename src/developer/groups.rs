use super::{DeveloperPortal, Endpoint, state::Transaction, validate_team};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Group {
    pub identifier: String,
    pub id: String,
    pub name: String,
}

pub(super) fn identifier(value: &str, group: bool) -> Result<()> {
    let segments: Vec<_> = value.split('.').collect();
    if value.len() > 255
        || segments.len() < 2
        || (group && !value.starts_with("group."))
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || segment.starts_with('-')
                || segment.ends_with('-')
        })
    {
        bail!("developer_identifier_invalid");
    }
    Ok(())
}

pub(super) fn resource(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        bail!("developer_resource_invalid");
    }
    Ok(())
}

pub(super) fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 100
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" _-".contains(&byte))
    {
        bail!("developer_name_invalid");
    }
    Ok(())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("developer_response_invalid"))
}

impl Group {
    fn parse(value: &Value) -> Result<Self> {
        let group = Self {
            identifier: text(value, "identifier")?.into(),
            id: text(value, "applicationGroup")?.into(),
            name: text(value, "name")?.into(),
        };
        identifier(&group.identifier, true)?;
        resource(&group.id)?;
        name(&group.name)?;
        Ok(group)
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Create,
    Associate,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub kind: Kind,
    pub team: String,
    pub identifier: String,
    pub name: Option<String>,
    pub bundle_id: Option<String>,
    pub app_id: Option<String>,
    pub group_id: Option<String>,
    pub generation: String,
    pub baseline: Vec<String>,
    pub expected: Vec<String>,
}

impl Plan {
    pub fn key(&self) -> (String, String, String) {
        (
            self.team.clone(),
            if self.kind == Kind::Create {
                "create"
            } else {
                "associate"
            }
            .into(),
            self.bundle_id
                .as_ref()
                .unwrap_or(&self.identifier)
                .to_ascii_lowercase(),
        )
    }
    pub fn valid(&self) -> bool {
        let ordered = |values: &[String]| {
            values.len() <= 1000
                && values.windows(2).all(|pair| pair[0] < pair[1])
                && values.iter().all(|value| resource(value).is_ok())
        };
        validate_team(&self.team).is_ok()
            && identifier(&self.identifier, true).is_ok()
            && self.generation.len() == 64
            && self.generation.bytes().all(|byte| byte.is_ascii_hexdigit())
            && ordered(&self.baseline)
            && ordered(&self.expected)
            && match self.kind {
                Kind::Create => {
                    self.name.as_ref().is_some_and(|value| name(value).is_ok())
                        && self.bundle_id.is_none()
                        && self.app_id.is_none()
                        && self.group_id.is_none()
                        && self.baseline.is_empty()
                        && self.expected.is_empty()
                }
                Kind::Associate => {
                    self.name.is_none()
                        && self
                            .bundle_id
                            .as_ref()
                            .is_some_and(|value| identifier(value, false).is_ok())
                        && self
                            .app_id
                            .as_ref()
                            .is_some_and(|value| resource(value).is_ok())
                        && self.group_id.as_ref().is_some_and(|value| {
                            resource(value).is_ok() && self.expected.contains(value)
                        })
                        && self
                            .baseline
                            .iter()
                            .all(|value| self.expected.contains(value))
                        && self.expected.len() <= self.baseline.len() + 1
                }
            }
    }
}

pub(super) struct WriteArgs {
    pub plan: Plan,
    pub dry_run: bool,
    pub operation_id: Option<String>,
    pub token: Option<String>,
}

fn object(args: &Value, allowed: &[&str], required: &[&str]) -> Result<()> {
    let fields = args
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))?;
    if fields.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !fields.contains_key(*key))
    {
        bail!("developer_arguments_invalid");
    }
    Ok(())
}

pub(super) fn team_args(args: &Value, bundle: bool) -> Result<(&str, Option<&str>)> {
    let allowed = if bundle {
        vec!["team_id", "bundle_id"]
    } else {
        vec!["team_id"]
    };
    object(args, &allowed, &allowed)?;
    let team = args["team_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))?;
    validate_team(team)?;
    let bundle = if bundle {
        let value = args["bundle_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))?;
        identifier(value, false)?;
        Some(value)
    } else {
        None
    };
    Ok((team, bundle))
}

pub(super) fn status_args(args: &Value, write: bool) -> Result<Option<&str>> {
    if !write {
        object(args, &[], &[])?;
        return Ok(None);
    }
    object(args, &["operation_id"], &["operation_id"])?;
    let value = args["operation_id"]
        .as_str()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))?;
    Ok(Some(value))
}

pub(super) fn write_args(args: &Value, kind: Kind) -> Result<WriteArgs> {
    let detail = if kind == Kind::Create {
        "name"
    } else {
        "bundle_id"
    };
    object(
        args,
        &[
            "team_id",
            "identifier",
            detail,
            "dry_run",
            "operation_id",
            "confirmation_token",
        ],
        &["team_id", "identifier", detail],
    )?;
    let string = |key: &str| {
        args[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))
    };
    let team = string("team_id")?;
    validate_team(&team)?;
    let group = string("identifier")?;
    identifier(&group, true)?;
    let value = string(detail)?;
    if kind == Kind::Create {
        name(&value)?;
    } else {
        identifier(&value, false)?;
    }
    let dry_run = match args.get("dry_run") {
        None => true,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("developer_arguments_invalid"))?,
    };
    let operation_id = args
        .get("operation_id")
        .map(|_| string("operation_id"))
        .transpose()?;
    let token = args
        .get("confirmation_token")
        .map(|_| string("confirmation_token"))
        .transpose()?;
    if (dry_run && (operation_id.is_some() || token.is_some()))
        || (!dry_run
            && (operation_id
                .as_ref()
                .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
                || token.as_ref().is_none_or(|value| {
                    value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })))
    {
        bail!("developer_confirmation_required");
    }
    let (name, bundle_id) = if kind == Kind::Create {
        (Some(value), None)
    } else {
        (None, Some(value))
    };
    Ok(WriteArgs {
        plan: Plan {
            kind,
            team,
            identifier: group,
            name,
            bundle_id,
            app_id: None,
            group_id: None,
            generation: String::new(),
            baseline: vec![],
            expected: vec![],
        },
        dry_run,
        operation_id,
        token,
    })
}

impl DeveloperPortal {
    async fn pages(
        &self,
        transaction: &mut Transaction,
        endpoint: Endpoint,
        key: &str,
        max: usize,
    ) -> Result<Vec<Value>> {
        let mut values = vec![];
        let mut total = None;
        let mut total_presence = None;
        for page in 1..=100 {
            let body = self
                .request(
                    transaction,
                    endpoint,
                    vec![
                        ("teamId".into(), transaction.source.team_id.clone()),
                        ("pageSize".into(), "100".into()),
                        ("pageNumber".into(), page.to_string()),
                    ],
                )
                .await?;
            let batch = body[key]
                .as_array()
                .filter(|batch| batch.len() <= 100)
                .ok_or_else(|| anyhow::anyhow!("developer_page_invalid"))?;
            let reported = match body.get("totalRecords") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|value| *value <= max as u64)
                        .ok_or_else(|| anyhow::anyhow!("developer_page_invalid"))?
                        as usize,
                ),
            };
            if total_presence.is_some_and(|present| present != reported.is_some())
                || total.is_some_and(|old| Some(old) != reported)
            {
                bail!("developer_page_changed");
            }
            total_presence = Some(reported.is_some());
            total = reported;
            values.extend(batch.iter().cloned());
            if values.len() > max || total.is_some_and(|total| values.len() > total) {
                bail!("developer_page_incomplete");
            }
            if batch.len() < 100 {
                if total.is_some_and(|total| total != values.len()) {
                    bail!("developer_page_incomplete");
                }
                return Ok(values);
            }
        }
        bail!("developer_page_incomplete")
    }

    pub(super) async fn groups(&self, transaction: &mut Transaction) -> Result<Vec<Group>> {
        transaction.csrf = None;
        let values = self
            .pages(transaction, Endpoint::Groups, "applicationGroupList", 1000)
            .await?;
        let mut ids = BTreeSet::new();
        let mut identifiers = BTreeSet::new();
        values
            .iter()
            .map(|value| {
                let item = Group::parse(value)?;
                if !ids.insert(item.id.clone())
                    || !identifiers.insert(item.identifier.to_ascii_lowercase())
                {
                    bail!("developer_identity_ambiguous");
                }
                Ok(item)
            })
            .collect()
    }

    fn find_group<'a>(&self, groups: &'a [Group], identifier: &str) -> Result<Option<&'a Group>> {
        match groups
            .iter()
            .find(|group| group.identifier.eq_ignore_ascii_case(identifier))
        {
            Some(group) if group.identifier != identifier => bail!("developer_identity_ambiguous"),
            result => Ok(result),
        }
    }

    async fn app_id(&self, transaction: &mut Transaction, bundle: &str) -> Result<String> {
        let apps = self
            .pages(transaction, Endpoint::Apps, "appIds", 10000)
            .await?;
        let mut ids = BTreeSet::new();
        let mut identifiers = BTreeSet::new();
        let mut result = None;
        for item in apps {
            let id = text(&item, "appIdId")?;
            resource(id)?;
            let identity = text(&item, "identifier")?;
            if !ids.insert(id.to_owned()) || !identifiers.insert(identity.to_ascii_lowercase()) {
                bail!("developer_identity_ambiguous");
            }
            if identity.eq_ignore_ascii_case(bundle) {
                if identity != bundle || item["appIdPlatform"].as_str() != Some("ios") {
                    bail!("developer_app_identity_invalid");
                }
                identifier(identity, false)?;
                result = Some(id.to_owned());
            }
        }
        result.ok_or_else(|| anyhow::anyhow!("developer_app_unavailable"))
    }

    pub(super) async fn associations(
        &self,
        transaction: &mut Transaction,
        bundle: &str,
        app: &str,
    ) -> Result<Vec<String>> {
        let body = self
            .request(
                transaction,
                Endpoint::Detail,
                vec![
                    ("teamId".into(), transaction.source.team_id.clone()),
                    ("appIdId".into(), app.into()),
                ],
            )
            .await?;
        let detail = &body["appId"];
        if text(detail, "appIdId")? != app
            || text(detail, "identifier")? != bundle
            || detail["appIdPlatform"].as_str() != Some("ios")
        {
            bail!("developer_app_identity_invalid");
        }
        if detail["features"]["APG3427HIY"].as_bool() != Some(true) {
            bail!("developer_capability_required");
        }
        let features = detail["enabledFeatures"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("developer_capability_unknown"))?;
        if !features.iter().all(Value::is_string)
            || !features
                .iter()
                .any(|value| value.as_str() == Some("APG3427HIY"))
        {
            bail!("developer_capability_unknown");
        }
        let values = detail["associatedApplicationGroups"]
            .as_array()
            .filter(|values| values.len() <= 1000)
            .ok_or_else(|| anyhow::anyhow!("developer_associations_unknown"))?;
        let mut ids = BTreeSet::new();
        let mut identifiers = BTreeSet::new();
        for value in values {
            let group = Group::parse(value)?;
            if !ids.insert(group.id) || !identifiers.insert(group.identifier.to_ascii_lowercase()) {
                bail!("developer_associations_unknown");
            }
        }
        if detail["associatedApplicationGroupsCount"].as_u64() != Some(ids.len() as u64) {
            bail!("developer_associations_incomplete");
        }
        Ok(ids.into_iter().collect())
    }

    pub(super) async fn resolve_plan(
        &self,
        transaction: &mut Transaction,
        request: &Plan,
    ) -> Result<(Plan, Option<Value>)> {
        let groups = self.groups(transaction).await?;
        let group = self.find_group(&groups, &request.identifier)?;
        let mut plan = request.clone();
        plan.generation = transaction.generation.clone();
        if plan.kind == Kind::Create {
            if let Some(group) = group {
                if Some(&group.name) != plan.name.as_ref() {
                    bail!("developer_existing_conflict");
                }
                return Ok((plan, Some(json!({"state":"no_op","group":group}))));
            }
        } else {
            let group = group.ok_or_else(|| anyhow::anyhow!("developer_group_unavailable"))?;
            let bundle = plan
                .bundle_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("developer_plan_invalid"))?;
            let app = self.app_id(transaction, bundle).await?;
            let baseline = self.associations(transaction, bundle, &app).await?;
            let mut expected = baseline.clone();
            if !expected.contains(&group.id) {
                expected.push(group.id.clone());
                expected.sort();
            }
            plan.app_id = Some(app);
            plan.group_id = Some(group.id.clone());
            plan.baseline = baseline;
            plan.expected = expected;
            if plan.baseline == plan.expected {
                return Ok((
                    plan.clone(),
                    Some(json!({"state":"no_op","group_ids":plan.expected})),
                ));
            }
        }
        Ok((plan, None))
    }

    pub(super) async fn read_groups_for_bundle(
        &self,
        transaction: &mut Transaction,
        bundle: &str,
    ) -> Result<Value> {
        let app = self.app_id(transaction, bundle).await?;
        let ids = self.associations(transaction, bundle, &app).await?;
        Ok(
            json!({"team_id":transaction.source.team_id,"bundle_id":bundle,"app_id":app,"group_ids":ids}),
        )
    }

    pub(super) async fn mutate(
        &self,
        transaction: &mut Transaction,
        plan: &Plan,
        started: std::time::Instant,
    ) -> Result<()> {
        if transaction.csrf.is_none() {
            bail!("developer_csrf_required");
        }
        let mut fields = vec![("teamId".into(), plan.team.clone())];
        if plan.kind == Kind::Create {
            fields.push(("identifier".into(), plan.identifier.clone()));
            fields.push((
                "name".into(),
                plan.name
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("developer_plan_invalid"))?,
            ));
            mutation_ready(transaction, started)?;
            let body = self
                .request(transaction, Endpoint::Create, fields)
                .await
                .map_err(mutation_error)?;
            let group = Group::parse(&body["applicationGroup"])?;
            if group.identifier != plan.identifier || Some(&group.name) != plan.name.as_ref() {
                bail!("developer_mutation_identity_unknown");
            }
            let readback = self
                .groups(transaction)
                .await
                .map_err(|_| anyhow::anyhow!("developer_readback_unknown"))?;
            if self.find_group(&readback, &plan.identifier)? != Some(&group) {
                bail!("developer_readback_mismatch");
            }
        } else {
            let bundle = plan
                .bundle_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("developer_plan_invalid"))?;
            let app = plan
                .app_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("developer_plan_invalid"))?;
            let baseline = self.associations(transaction, bundle, app).await?;
            if baseline != plan.baseline {
                bail!("developer_baseline_changed");
            }
            fields.push(("appIdId".into(), app.clone()));
            fields.push(("displayId".into(), app.clone()));
            fields.extend(
                plan.expected
                    .iter()
                    .map(|value| ("applicationGroups[]".into(), value.clone())),
            );
            mutation_ready(transaction, started)?;
            let body = self
                .request(transaction, Endpoint::Associate, fields)
                .await
                .map_err(mutation_error)?;
            if !body.is_object() {
                bail!("developer_response_invalid");
            }
            if self
                .associations(transaction, bundle, app)
                .await
                .map_err(|_| anyhow::anyhow!("developer_readback_unknown"))?
                != plan.expected
            {
                bail!("developer_readback_mismatch");
            }
        }
        Ok(())
    }

    pub(super) async fn observe(&self, transaction: &mut Transaction, plan: &Plan) -> Result<bool> {
        if plan.kind == Kind::Create {
            let groups = self.groups(transaction).await?;
            Ok(self
                .find_group(&groups, &plan.identifier)?
                .is_some_and(|group| Some(&group.name) == plan.name.as_ref()))
        } else {
            let app = self
                .app_id(
                    transaction,
                    plan.bundle_id
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("developer_plan_invalid"))?,
                )
                .await?;
            if Some(&app) != plan.app_id.as_ref() {
                bail!("developer_app_identity_invalid");
            }
            Ok(self
                .associations(
                    transaction,
                    plan.bundle_id.as_deref().unwrap_or_default(),
                    &app,
                )
                .await?
                == plan.expected)
        }
    }
}

fn mutation_ready(transaction: &Transaction, started: std::time::Instant) -> Result<()> {
    if started.elapsed() > std::time::Duration::from_secs(90) {
        bail!("developer_preflight_deadline");
    }
    transaction.ensure_generation()
}

fn mutation_error(error: anyhow::Error) -> anyhow::Error {
    if error.to_string() == "developer_rejected" {
        anyhow::anyhow!("developer_mutation_rejected")
    } else {
        anyhow::anyhow!("developer_mutation_unknown")
    }
}
