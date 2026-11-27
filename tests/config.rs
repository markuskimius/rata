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
name = "WEB_API"
description = "Web API Server"
start = "${RATA}/bin/api --port 8080"

[[service]]
name = "_WORKER2"
start = ["java", "-jar", "worker.jar"]
psline = "java -jar worker.jar"
stop_timeout = 30
"#,
    )
    .unwrap();
    assert_eq!(c.version, 1);
    assert_eq!(c.services.len(), 2);
    let w = &c.services[1];
    assert_eq!(w.name, "_WORKER2");
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
    let long = "A".repeat(65);
    assert!(
        err(&format!("[[service]]\nname = \"{long}\"\nstart = \"x\"\n")).contains("longer than 64")
    );
    assert!(
        parse(&format!(
            "[[service]]\nname = \"{}\"\nstart = \"x\"\n",
            "A".repeat(64)
        ))
        .is_ok()
    );
}

#[test]
fn duplicate_names_report_both_lines() {
    let e =
        err("[[service]]\nname = \"A\"\nstart = \"x\"\n[[service]]\nname = \"A\"\nstart = \"y\"\n");
    assert_eq!(
        e,
        "service.toml:5:8: duplicate service name \"A\" (first defined on line 2)"
    );
}

#[test]
fn field_errors() {
    assert!(err("[[service]]\nname = \"A\"\n").contains("missing field `start`"));
    assert!(err("[[service]]\nname = \"A\"\nstart = \"\"\n").contains("start: command is empty"));
    assert!(err("[[service]]\nname = \"A\"\nstart = []\n").contains("start: command is empty"));
    assert!(
        err("[[service]]\nname = \"A\"\nstart = 5\n").contains("a string or an array of strings")
    );
    assert!(
        err("[[service]]\nname = \"A\"\nstart = \"x\"\npsline = \"\\\"oops\"\n")
            .contains("psline: missing closing quote")
    );
    assert!(
        err("[[service]]\nname = \"A\"\nstart = \"x\"\ndescription = \"two\\nlines\"\n")
            .contains("single line")
    );
    assert!(
        err("[[service]]\nname = \"A\"\nstart = \"x\"\nstop_timeout = 0\n")
            .contains("between 1 and 3600")
    );
    assert!(
        err("[[service]]\nname = \"A\"\nstart = \"x\"\nbogus = 1\n")
            .contains("unknown field `bogus`")
    );
    assert!(err("version = 2\n").contains("unsupported config version 2"));
    assert!(err("verison = 1\n").contains("unknown field `verison`"));
}

#[test]
fn select_keeps_config_order() {
    let c = parse(
        "[[service]]\nname = \"A\"\nstart = \"x\"\n[[service]]\nname = \"B\"\nstart = \"y\"\n",
    )
    .unwrap();
    let names = |v: Vec<&rata::Service>| v.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(c.select(&[]).unwrap()), ["A", "B"]);
    assert_eq!(
        names(c.select(&["B".into(), "A".into()]).unwrap()),
        ["A", "B"]
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

#[test]
fn names_match_ignoring_case_when_unique() {
    let c = parse(
        "[[service]]\nname = \"WEB_API\"\nstart = \"x\"\n\
         [[service]]\nname = \"Cache\"\nstart = \"y\"\n\
         [[service]]\nname = \"CACHE\"\nstart = \"z\"\n",
    )
    .unwrap();
    assert_eq!(c.service("web_api").unwrap().name, "WEB_API");
    assert_eq!(c.service("Web_Api").unwrap().name, "WEB_API");
    // An exact match wins over case-insensitive ones.
    assert_eq!(c.service("Cache").unwrap().name, "Cache");
    assert_eq!(c.service("CACHE").unwrap().name, "CACHE");
    let e = c.select(&["cache".into()]).unwrap_err().to_string();
    assert_eq!(
        e,
        "service name \"cache\" is ambiguous: it matches Cache, CACHE (type the name exactly)"
    );
    // The same service named twice is selected once.
    assert_eq!(
        c.select(&["web_api".into(), "WEB_API".into()])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn explicit_flags_and_selection() {
    use rata::config::{Action, SkipReason};
    let c = parse(
        "[[service]]\nname = \"MON\"\nstart = \"m\"\nexplicit_stop = true\n\
         [[service]]\nname = \"WEB\"\nstart = \"w\"\n\
         [[service]]\nname = \"BATCH\"\nstart = \"b\"\nexplicit_start = true\n",
    )
    .unwrap();
    assert!(c.services[0].explicit_stop && !c.services[0].explicit_start);
    assert!(c.services[2].explicit_start && !c.services[2].explicit_stop);

    let run = |action, names: &[&str], all| {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let sel = c.select_for(action, &names, all).unwrap();
        let on: Vec<String> = sel.services.iter().map(|s| s.name.clone()).collect();
        let skipped: Vec<(String, SkipReason)> = sel
            .skipped
            .iter()
            .map(|s| (s.service.name.clone(), s.reason))
            .collect();
        (on, skipped)
    };
    assert_eq!(
        run(Action::Start, &[], false),
        (
            vec!["MON".into(), "WEB".into()],
            vec![("BATCH".into(), SkipReason::ExplicitStart)]
        )
    );
    assert_eq!(
        run(Action::Stop, &[], false),
        (
            vec!["WEB".into(), "BATCH".into()],
            vec![("MON".into(), SkipReason::ExplicitStop)]
        )
    );
    assert_eq!(
        run(Action::Restart, &[], false),
        (
            vec!["WEB".into()],
            vec![
                ("MON".into(), SkipReason::ExplicitStop),
                ("BATCH".into(), SkipReason::ExplicitStart)
            ]
        )
    );
    for action in [Action::Start, Action::Stop, Action::Restart] {
        assert_eq!(run(action, &[], true).0.len(), 3);
        assert!(run(action, &[], true).1.is_empty());
        assert_eq!(run(action, &["mon", "batch"], false).0, ["MON", "BATCH"]);
    }
    assert!(
        err("[[service]]\nname = \"A\"\nstart = \"x\"\nexplicit_stop = \"yes\"\n")
            .contains("invalid type")
    );
}
