use boa_engine::{Context, Source};

fn main() {
    let mut context = Context::default();

    let json_str = r#"
    {
        "Search Verse": {
            "output": {
                "matches": [
                    { "row": 75 }
                ]
            },
            "data": {
                "matches": [
                    { "row": 75 }
                ]
            }
        }
    }
    "#;

    let setup_script = format!("var $node = {};", json_str);
    if let Err(e) = context.eval(Source::from_bytes(setup_script.as_bytes())) {
        println!("Setup Error: {}", e);
        return;
    }

    let expression = r#"$node["Search Verse"].data.matches[0].row + 1"#;
    let wrapped = format!("(function() {{ return {}; }})()", expression);

    match context.eval(Source::from_bytes(wrapped.as_bytes())) {
        Ok(res) => println!("Success: {:?}", res.display()),
        Err(e) => println!("JS Error: {}", e),
    }
}
