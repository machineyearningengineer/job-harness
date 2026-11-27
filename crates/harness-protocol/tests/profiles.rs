use harness_core::{contracts, workspace};
use job_harness_protocol::{
    FOCUSED_TOOLS,
    mcp::{Backend, McpServer},
};
use serde_json::{Value, json};

#[test]
fn focused_profile_defers_specialized_tools_with_actionable_next_step() {
    let temp = tempfile::tempdir().unwrap();
    workspace::initialize(temp.path()).unwrap();
    let specs = Value::Object(contracts::specs().clone());
    let server = McpServer::new(temp.path(), "focused", &specs).unwrap();
    assert_eq!(FOCUSED_TOOLS.len(), 13);
    assert!(contracts::specs().len() >= 30);
    assert_eq!(server.invoke("bootstrap", &json!({}))["ok"], true);
    let deferred = server.invoke("find_application", &json!({}));
    assert_eq!(deferred["error"]["code"], "deferred_tool");
    assert_eq!(deferred["next"][0]["tool"], "inspect_tools");
    assert_eq!(
        server.invoke("unknown", &json!({}))["error"]["code"],
        "unknown_tool"
    );
    let full = McpServer::new(temp.path(), "full", &specs).unwrap();
    assert_eq!(full.invoke("find_application", &json!({}))["ok"], true);
    let restricted = McpServer::with_policy(temp.path(), "full", &specs, true).unwrap();
    assert!(rmcp::ServerHandler::get_tool(&restricted, "begin_submission").is_none());
    assert!(rmcp::ServerHandler::get_tool(&server, "begin_mail_archive").is_some());
    assert_eq!(
        server.invoke("begin_mail_archive", &json!({}))["error"]["code"],
        "invalid_arguments"
    );
    assert!(rmcp::ServerHandler::get_tool(&restricted, "begin_mail_archive").is_none());
    assert!(rmcp::ServerHandler::get_tool(&restricted, "accept_note").is_none());
    assert!(rmcp::ServerHandler::get_tool(&restricted, "query_batch").is_some());
    assert_eq!(
        restricted.invoke("record_event", &json!({"tracking":{"stage":"saved"}}))["error"]["code"],
        "tool_denied"
    );
    for tool in [
        "begin_submission",
        "begin_mail_archive",
        "finish_submission",
        "update_application",
        "accept_note",
        "archive_note",
    ] {
        assert_eq!(
            restricted.invoke(tool, &json!({}))["error"]["code"],
            "tool_denied"
        );
        assert_eq!(
            restricted.invoke(
                "execute_batch",
                &json!({"steps":[{"id":"hidden","tool":tool,"arguments":{}}]})
            )["error"]["code"],
            "tool_denied"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_backend_applies_the_same_profile_through_jh_serve() {
    let temp = tempfile::tempdir().unwrap();
    workspace::initialize(temp.path()).unwrap();
    let token = "0123456789abcdef0123".to_owned();
    let (app, _watcher) = job_harness_protocol::remote::router(temp.path(), token.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let client = job_harness_protocol::remote::Client::new(&url, token).unwrap();
    let specs = Value::Object(contracts::specs().clone());
    let server = McpServer::with_backend(Backend::Remote(client), "focused", &specs, true).unwrap();
    let (boot, deferred, denied) = tokio::task::spawn_blocking(move || {
        (
            server.invoke("bootstrap", &json!({})),
            server.invoke("find_application", &json!({})),
            server.invoke("begin_submission", &json!({})),
        )
    })
    .await
    .unwrap();
    assert_eq!(boot["ok"], true, "{boot}");
    assert_eq!(deferred["error"]["code"], "deferred_tool");
    assert_eq!(denied["error"]["code"], "tool_denied");
}
