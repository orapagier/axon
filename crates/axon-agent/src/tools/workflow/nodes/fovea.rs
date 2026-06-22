use crate::state::AppState;
use serde_json::Value;

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let mut args = serde_json::Map::new();
    if let Some(obj) = config.as_object() {
        for (k, v) in obj {
            args.insert(k.clone(), v.clone());
        }
    }
    match crate::agent::r#loop::execute_internal_tool_from_workflow(
        "image_tool",
        Value::Object(args),
        state.clone(),
    )
    .await
    {
        Ok(v) => Ok(v),
        Err(e) => Err(e.to_string()),
    }
}
