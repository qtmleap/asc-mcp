use asc_mcp::{client::ApiClient, uploads};
use httpmock::{Method::GET, MockServer};
use serde_json::json;

#[tokio::test]
async fn reports_preserve_opaque_gzip_bytes_and_filter_names() {
    let server = MockServer::start_async().await;
    let sales = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/salesReports")
                .query_param("filter[vendorNumber]", "123")
                .query_param("filter[reportType]", "SALES")
                .query_param("filter[reportSubType]", "SUMMARY")
                .query_param("filter[frequency]", "DAILY")
                .query_param("filter[reportDate]", "2026-10-01");
            then.status(200)
                .header("content-type", "application/a-gzip")
                .body(vec![0x1f, 0x8b, 0x08, 0x00]);
        })
        .await;
    let finance = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/v1/financeReports")
                .query_param("filter[vendorNumber]", "123")
                .query_param("filter[reportType]", "FINANCIAL")
                .query_param("filter[regionCode]", "JP")
                .query_param("filter[reportDate]", "2026-10");
            then.status(200).body("opaque TSV");
        })
        .await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    let output=uploads::handle(&client,"get_sales_report",&json!({"vendor_number":"123","report_type":"SALES","report_sub_type":"SUMMARY","frequency":"DAILY","report_date":"2026-10-01"})).await.unwrap().unwrap();
    assert!(
        output["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("4 bytes")
    );
    let output=uploads::handle(&client,"get_finance_report",&json!({"vendor_number":"123","report_type":"FINANCIAL","region_code":"JP","report_date":"2026-10"})).await.unwrap().unwrap();
    assert!(
        output["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("opaque TSV")
    );
    sales.assert_async().await;
    finance.assert_async().await;
}
#[tokio::test]
async fn helpers_reject_invalid_arguments_and_defer_unrelated_tools() {
    let server = MockServer::start_async().await;
    let client = ApiClient::with_endpoint(&server.base_url()).unwrap();
    assert!(
        uploads::handle(&client, "get_apps", &json!({}))
            .await
            .unwrap()
            .is_none()
    );
    for args in [
        json!({}),
        json!({"screenshot_set_id":"../bad","file_name":"x","file_data_base64":"YQ=="}),
        json!({"screenshot_set_id":"set","file_name":"x","file_path":"x","file_data_base64":"YQ=="}),
        json!({"screenshot_set_id":"set","file_name":"x","file_data_base64":"invalid base64"}),
    ] {
        assert!(
            uploads::handle(&client, "upload_app_screenshot", &args)
                .await
                .is_err()
        );
    }
}
