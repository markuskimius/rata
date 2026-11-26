//! Config parsing and validation.

use std::path::Path;

use rata::{CommandSpec, Config, Error};

fn parse(text: &str) -> Result<Config, Error> {
    Config::parse(text, Path::new("service.toml"))
}

fn err(text: &str) -> String {
    parse(text).unwrap_err().to_string()
}

#[test]
fn full_example_parses() {
    let c = parse(
        r#"
version = 1

[[service]]
name = "web_api"
description = "Web API Server"
start = "${RATA}/bin/api --port 8080"

[[service]]
name = "_worker2"
start = ["java", "-jar", "worker.jar"]
psline = "java -jar worker.jar"
stop_timeout = 30
"#,
    )
    .unwrap();
    assert_eq!(c.version, 1);
    assert_eq!(c.services.len(), 2);
    let w = &c.services[1];
    assert_eq!(w.name, "_worker2");
    assert_eq!(
        w.start,
        CommandSpec::Args(vec!["java".into(), "-jar".into(), "worker.jar".into()])
    );
    assert_eq!(
        w.psline,
        Some(CommandSpec::Line("java -jar worker.jar".into()))
    );
    assert_eq!(w.stop_timeout().as_secs(), 30);
    assert_eq!(c.services[0].stop_timeout().as_secs(), 10);
}

#[test]
fn empty_config_is_valid() {
    assert!(parse("").unwrap().services.is_empty());
}

#[test]
fn names() {
    for bad in ["1abc", "a-b", "a b", "", "é"] {
        let e = err(&format!("[[service]]\nname = \"{bad}\"\nstart = \"x\"\n"));
        assert!(e.contains("invalid service name"), "{bad}: {e}");
    }
    let long = "a".repeat(65);
    assert!(
        err(&format!("[[service]]\nname = \"{long}\"\nstart = \"x\"\n")).contains("longer than 64")
    );
    assert!(
        parse(&format!(
            "[[service]]\nname = \"{}\"\nstart = \"x\"\n",
            "a".repeat(64)
        ))
        .is_ok()
    );
}

#[test]
fn duplicate_names_report_both_lines() {
    let e =
        err("[[service]]\nname = \"a\"\nstart = \"x\"\n[[service]]\nname = \"a\"\nstart = \"y\"\n");
    assert_eq!(
        e,
        "service.toml:5:8: duplicate service name \"a\" (first defined on line 2)"
    );
}

#[test]
fn field_errors() {
    assert!(err("[[service]]\nname = \"a\"\n").contains("missing field `start`"));
    assert!(err("[[service]]\nname = \"a\"\nstart = \"\"\n").contains("start: command is empty"));
    assert!(err("[[service]]\nname = \"a\"\nstart = []\n").contains("start: command is empty"));
    assert!(
        err("[[service]]\nname = \"a\"\nstart = 5\n").contains("a string or an array of strings")
    );
    assert!(
        err("[[service]]\nname = \"a\"\nstart = \"x\"\npsline = \"'oops\"\n")
            .contains("psline: missing closing quote")
    );
    assert!(
        err("[[service]]\nname = \"a\"\nstart = \"x\"\ndescription = \"two\\nlines\"\n")
            .contains("single line")
    );
    assert!(
        err("[[service]]\nname = \"a\"\nstart = \"x\"\nstop_timeout = 0\n")
            .contains("between 1 and 3600")
    );
    assert!(
        err("[[service]]\nname = \"a\"\nstart = \"x\"\nbogus = 1\n")
            .contains("unknown field `bogus`")
    );
    assert!(err("version = 2\n").contains("unsupported config version 2"));
    assert!(err("verison = 1\n").contains("unknown field `verison`"));
}

#[test]
fn select_keeps_config_order() {
    let c = parse(
        "[[service]]\nname = \"a\"\nstart = \"x\"\n[[service]]\nname = \"b\"\nstart = \"y\"\n",
    )
    .unwrap();
    let names = |v: Vec<&rata::Service>| v.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(c.select(&[]).unwrap()), ["a", "b"]);
    assert_eq!(
        names(c.select(&["b".into(), "a".into()]).unwrap()),
        ["a", "b"]
    );
    assert!(matches!(
        c.select(&["z".into()]),
        Err(Error::UnknownService { .. })
    ));
}

#[test]
fn shipped_example_is_valid() {
    let c = parse(include_str!("../docs/service.example.toml")).unwrap();
    assert_eq!(c.services.len(), 3);
}
