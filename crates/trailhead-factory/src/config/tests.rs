use super::*;
use GateKind::{AgentReview, Checks, Ci, Human};

fn gr(n: u8) -> Grade {
    Grade::new(n).unwrap()
}

#[test]
fn stack_env_reaches_the_workers_prepare_and_the_tag_only_when_set() {
    let with = SAMPLE.to_string();
    let config =
        parse(&with.replace(
            "tools = { rust = \"stable\" }",
            "tools = { rust = \"stable\" }\nenv = { CARGO_BUILD_JOBS = \"2\" }",
        ))
        .unwrap();
    assert_eq!(config.stack.env["CARGO_BUILD_JOBS"], "2");
    assert_eq!(config.prepare_request(Role::Worker, None).env, config.stack.env);
    assert!(config.prepare_request(Role::Reviewer, None).env.is_empty());
    let plain = parse(&with).unwrap();
    assert_ne!(config.stack.tag(), plain.stack.tag(), "setup runs with the env, so it can change the checkpoint");
    assert_eq!(plain.stack.tag(), Stack { tools: plain.stack.tools, env: Default::default() }.tag());
}

#[test]
fn stack_env_refuses_the_names_the_factory_sets() {
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "TRAILHEAD_FACTORY_TASK",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "MISE_OFFLINE",
        "PATH",
        "A=B",
    ] {
        let src = SAMPLE.replace("tools = { rust = \"stable\" }", &format!("env = {{ \"{name}\" = \"x\" }}"));
        let err = parse(&src).unwrap_err().to_string();
        assert!(err.contains("[factory.stack] env") && err.contains(name), "{name}: {err}");
    }
}

#[test]
fn the_stack_tag_follows_the_tools() {
    let stack = |pairs: &[(&str, &str)]| Stack {
        tools: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        env: Default::default(),
    };
    assert_eq!(stack(&[]).tag(), "");
    let a = stack(&[("node", "26")]).tag();
    assert_eq!(a, stack(&[("node", "26")]).tag());
    assert_ne!(a, stack(&[("node", "24")]).tag());
    assert_ne!(a, stack(&[("node", "26"), ("rust", "stable")]).tag());
}

#[test]
fn gate_kinds_keep_their_string_form() {
    let kinds = [
        (GateKind::Checks, "checks"),
        (GateKind::Ci, "ci"),
        (GateKind::AgentReview, "agent_review"),
        (GateKind::Human, "human"),
        (GateKind::Quorum, "quorum"),
        (GateKind::Named("lint".into()), "lint"),
    ];
    for (kind, text) in kinds {
        assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<GateKind>(&format!("\"{text}\"")).unwrap(), kind);
    }
}

fn parse(factory_src: &str) -> Result<FactoryConfig> {
    FactoryConfig::parse_table(factory_src, "TRAIL", None)
}

#[test]
fn the_repos_own_trailhead_toml_parses() {
    let remote = Repo { owner: "lanesawyer".into(), name: "trailhead".into() };
    let config = FactoryConfig::from_repo_file(include_str!("../../../../.trailhead.toml"), Some(remote)).unwrap();
    assert_eq!(config.repo.project, "TRAIL");
    let prepare = config.prepare_request(Role::Worker, None);
    assert!(prepare.trim.unwrap().command.contains("trim-cargo"));
    assert_eq!(prepare.disk_warn_bytes, Some(DEFAULT_DISK_WARN_BYTES));
    assert_eq!(prepare.disk_limit_bytes, None);
    assert!(config.prepare_request(Role::Reviewer, None).trim.is_none(), "reviewers don't build");
    assert!(config.prepare_request(Role::Reviewer, None).tools.is_empty(), "reviewers don't install the stack");
    assert!(config.prepare_request(Role::Reviewer, None).tool_hosts.is_empty());
    assert_eq!(config.prepare_request(Role::Worker, None).tools, config.stack.tools);
    let mut documented = GradesConfig::default();
    documented.rules.get_mut("2").unwrap().push("clients/**".into());
    documented.rules.get_mut("3").unwrap().push("clients/*/src/**".into());
    assert_eq!(
        config.grades, documented,
        "this repo's rules are the documented example, plus clients/** at 2 and its src at 3 (TRAIL-1390)"
    );
    assert_eq!(config.merge.mode, MergeMode::On);
    assert!(config.gate(gr(1)).auto_merge);
    assert!(config.gate(gr(2)).auto_merge);
    assert!(!config.gate(gr(3)).auto_merge);
    assert_eq!(config.gate(gr(3)).require, [Checks, Ci, AgentReview, Human]);
    let raised = |path: &str| {
        config
            .grades
            .rules
            .iter()
            .filter(|(_, g)| g.iter().any(|g| crate::grade::glob_match(g, path)))
            .map(|(n, _)| n.as_str())
            .max()
    };
    assert_eq!(raised("crates/trailhead-core/tests/domain.rs"), None, "test-only changes stay at the task's grade");
    assert_eq!(raised("crates/trailhead-core/src/tasks.rs"), Some("2"));
    assert_eq!(raised("AGENTS.md"), Some("4"));
    assert_eq!(raised(".claude/settings.json"), Some("4"));
}

#[test]
fn queue_is_a_key_or_a_list_of_them() {
    let with = |q: &str| parse(&SAMPLE.replace("[repo]\n", &format!("[repo]\n{q}\n"))).unwrap().repo.queue;
    assert!(parse(SAMPLE).unwrap().repo.queue.is_empty());
    assert_eq!(with("queue = \"TRAIL-102\""), ["TRAIL-102"]);
    assert_eq!(with("queue = [\"TRAIL-102\", \"TRAIL-167\"]"), ["TRAIL-102", "TRAIL-167"]);
    let err = parse(&SAMPLE.replace("[repo]\n", "[repo]\nqueue = []\n")).unwrap_err();
    assert!(err.to_string().contains("queue = [] is empty"), "{err}");
}
#[test]
fn lockfiles_are_grade_four_and_manifests_grade_three_by_default() {
    let g = GradesConfig::default();
    let grade_of = |path: &str| {
        g.rules
            .iter()
            .filter(|(_, gl)| gl.iter().any(|p| crate::grade::glob_match(p, path)))
            .map(|(k, _)| k.as_str())
            .max()
    };
    for lock in [
        "Cargo.lock",
        "web/package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "uv.lock",
        "poetry.lock",
        "go.sum",
        "Gemfile.lock",
    ] {
        assert_eq!(grade_of(lock), Some("4"), "{lock}");
    }
    assert_eq!(grade_of("crates/a/Cargo.toml"), Some("3"));
}
#[test]
fn grades_default_to_the_documented_rules_and_parse_from_the_file() {
    let base = SAMPLE;
    let config = parse(base).unwrap();
    assert_eq!(config.grades, GradesConfig::default());

    let custom = format!("{base}\n[grades]\n3 = [\"Cargo.lock\"]\nmax_lines = 50\n");
    let config = parse(&custom).unwrap();
    assert_eq!(config.grades.max_lines, 50);
    assert_eq!(config.grades.rules.keys().collect::<Vec<_>>(), ["3"]);
    assert_eq!(config.grades.grader().rules.len(), 1);

    let no_limit = format!("{base}\n[grades]\n2 = [\"src/**\"]\n");
    assert_eq!(parse(&no_limit).unwrap().grades.max_lines, 600);

    for bad in ["1 = [\"a\"]", "5 = [\"a\"]", "x = [\"a\"]", "max_line = 5"] {
        let err = parse(&format!("{base}\n[grades]\n{bad}\n")).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
    }
    let err = parse(&format!("{base}\n[grades]\n5 = [\"a\"]\n")).unwrap_err();
    assert!(err.to_string().contains("[factory.grades] has `5`"), "{err}");

    let ok = format!("{base}\n[grades]\n3 = [\"crates/{{core,server}}/**\", \"src/[ab].rs\"]\n");
    assert!(parse(&ok).is_ok());
    let err = parse(&format!("{base}\n[grades]\n3 = [\"src/[ab\"]\n")).unwrap_err();
    assert!(err.to_string().contains("bad glob `src/[ab`"), "{err}");
}

const SAMPLE: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"

[stack]
tools = { rust = "stable" }

[checks]
fmt = "cargo fmt --check"
lint = "cargo clippy -- -D warnings"
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = { usd = 5, minutes = 60, steps = 40 }
escalate_to = "worker-opus"

[profiles.worker-opus]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 15, minutes = 90, steps = 60 }

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 2, minutes = 20, steps = 20 }

[gates.1]
require = ["checks", "agent_review", "human"]
"#;

#[test]
fn parses_the_documented_shape() {
    let c = parse(SAMPLE).unwrap();
    assert_eq!(c.repo.base_branch, "main");
    assert_eq!(c.worker().model, ModelAlias::Sonnet);
    assert_eq!(c.worker().budget.usd(), Usd::dollars(5.0));
    let names: Vec<_> = c.required_checks().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["fmt", "lint", "test"]);
}

#[test]
fn shots_are_a_command_and_the_paths_that_call_for_it() {
    assert_eq!(parse(SAMPLE).unwrap().shots(), None, "a repo without the table takes none");
    let with = format!("{SAMPLE}\n[shots]\nrun = \"make shots\"\npaths = [\"clients/**\"]\n");
    let shots = parse(&with).unwrap().shots().unwrap();
    assert_eq!(shots.command, "make shots");
    assert!(shots.applies_to(&["clients/app/src/view.rs".to_string()]));
    assert!(!shots.applies_to(&["crates/a/src/lib.rs".to_string()]));
    let always = parse(&format!("{SAMPLE}\n[shots]\nrun = \"make shots\"\n")).unwrap().shots().unwrap();
    assert!(always.applies_to(&[]), "no paths means every change");
    let empty = parse(&format!("{SAMPLE}\n[shots]\nrun = \" \"\n")).unwrap_err();
    assert!(empty.to_string().contains("[factory.shots] run is empty"), "{empty}");
}

#[test]
fn a_check_is_a_string_or_a_table_with_paths() {
    let c = parse(&SAMPLE.replace(
        "test = \"cargo test\"",
        "test = { run = \"cargo test\", paths = [\"crates/**\"] }\nui-lint = { run = \"cargo clippy -p ui\", paths = [\"clients/**\"] }\nzz = \"true\"",
    ))
    .unwrap();
    let checks = c.required_checks();
    let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["fmt", "lint", "test", "ui-lint", "zz"]);
    assert!(checks[0].paths.is_empty() && checks[4].paths.is_empty(), "a string runs always");
    assert_eq!(checks[2].paths, ["crates/**"]);
    assert_eq!(checks[3].command, "cargo clippy -p ui");
    assert!(checks[3].applies_to(&["clients/lookout/src/main.rs".to_string()]));
    assert!(!checks[3].applies_to(&["crates/a/src/lib.rs".to_string()]));
    assert!(checks[0].applies_to(&[]));
    // A gate can require a scoped check by name.
    let gated = SAMPLE.replace("\"checks\", \"agent_review\"", "\"ui-lint\", \"agent_review\"");
    assert!(parse(&gated.replace("test = ", "ui-lint = \"x\"\ntest = ")).is_ok());
}

#[test]
fn rejects_extra_checks_named_like_a_misspelled_reserved_key() {
    for typo in ["lnt", "fmt-fix", "setp", "lnit", "tset", "stup", "bulid", "tests", "typecheck2", "Build"] {
        let bad = SAMPLE.replace("test = ", &format!("{typo} = \"x\"\ntest = "));
        let err = parse(&bad).unwrap_err();
        assert!(err.to_string().contains("misspelling"), "{typo}: {err}");
    }
    for ok in ["ui-lint", "e2e", "docs", "audit"] {
        assert!(parse(&SAMPLE.replace("test = ", &format!("{ok} = \"x\"\ntest = "))).is_ok(), "{ok}");
    }
}

#[test]
fn rejects_a_scoped_check_with_unknown_keys() {
    let bad = SAMPLE.replace("test = \"cargo test\"", "test = { run = \"t\", path = [\"a\"] }");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
}

#[test]
fn pause_at_is_the_user_configs_not_the_repos() {
    let err = parse(&format!("{SAMPLE}\n[usage]\npause_at = {{ five_hour = 80 }}\n")).unwrap_err();
    assert!(err.to_string().contains("usage"), "{err}");
}

#[test]
fn workers_run_every_check_whatever_the_gates_require() {
    let c =
        parse(&SAMPLE.replace("require = [\"checks\", \"agent_review\", \"human\"]", "require = [\"ci\"]")).unwrap();
    let names: Vec<_> = c.required_checks().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["fmt", "lint", "test"]);
}

#[test]
fn rejects_zero_stall_timeout() {
    let bad = SAMPLE.replace("escalate_to = \"worker-opus\"", "escalate_to = \"worker-opus\"\nstall_timeout = 0");
    match parse(&bad) {
        Err(Error::Config(m)) => assert!(m.contains("stall_timeout"), "{m}"),
        other => panic!("expected a config error, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_profiles_and_gate_requirements() {
    let bad = SAMPLE.replace("escalate_to = \"worker-opus\"", "escalate_to = \"nope\"");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
    let bad = SAMPLE.replace("\"agent_review\", \"human\"", "\"agent_review\", \"vibes\"");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
}

#[test]
fn gates_default_to_the_table_and_merge_mode_to_off() {
    let mut c = parse(&SAMPLE.replace("[gates.1]\nrequire = [\"checks\", \"agent_review\", \"human\"]", "")).unwrap();
    assert_eq!(c.merge.mode, MergeMode::Off);
    assert_eq!(c.gate(gr(1)).require, [Checks, Ci]);
    assert!(c.gate(gr(2)).auto_merge);
    assert_eq!(c.gate(gr(3)).require, [Checks, Ci, AgentReview, Human]);
    assert!(!c.gate(gr(3)).auto_merge);
    assert_eq!(c.gate(gr(4)).require, [Checks, Ci, Human]);

    let custom = format!("{SAMPLE}\n[gates.2]\nrequire = [\"ci\"]\nauto_merge = true\n[merge]\nmode = \"shadow\"\n");
    c = parse(&custom).unwrap();
    assert_eq!(c.gate(gr(2)).require, [Ci]);
    assert_eq!(c.gate(gr(1)).require, [Checks, AgentReview, Human]);
    assert_eq!(c.merge.mode, MergeMode::Shadow);
}

#[test]
fn merge_mode_on_is_accepted_and_gates_for_unknown_grades_are_rejected() {
    let on = format!("{SAMPLE}\n[merge]\nmode = \"on\"\n");
    assert_eq!(parse(&on).unwrap().merge.mode, MergeMode::On);
    for bad in ["[merge]\nmode = \"yes\"", "[gates.t1]\nrequire = []", "[gates.5]\nrequire = []"] {
        let err = parse(&format!("{SAMPLE}\n{bad}\n")).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{bad}: {err}");
    }
    let err = parse(&format!("{SAMPLE}\n[gates.5]\nrequire = []\n")).unwrap_err();
    assert!(err.to_string().contains("[factory.gates.5]: gates are per grade"), "{err}");
}

#[test]
fn profile_and_gate_errors_name_the_table_to_edit() {
    let cases = [
        (SAMPLE.replace("[repo]\n", "[repo]\nworker = \"nope\"\n"), "[factory.repo] worker: no profile named nope"),
        (
            SAMPLE.replace("[repo]\n", "[repo]\nreviewer = \"worker\"\n"),
            "[factory.repo] reviewer is `worker`, but [factory.profiles.worker] has role worker, not reviewer",
        ),
        (
            SAMPLE.replace("escalate_to = \"worker-opus\"", "escalate_to = \"ghost\""),
            "[factory.profiles.worker] escalate_to is `ghost`, but there is no [factory.profiles.ghost]",
        ),
        (format!("{SAMPLE}\n[gates.2]\nrequire = [\"lintt\"]\n"), "[factory.gates.2] requires unknown lintt"),
        (
            format!("{SAMPLE}\n[gates.2]\nrequire = [\"quorum\"]\n"),
            "[factory.gates.2] requires a quorum but doesn't define one",
        ),
    ];
    for (src, want) in cases {
        let err = parse(&src).unwrap_err().to_string();
        assert!(err.contains(want), "want {want:?}, got {err}");
    }
}

#[test]
fn a_compute_table_is_one_default_target() {
    let c = parse(SAMPLE).unwrap();
    assert!(c.targets.is_empty());
    let specs = c.target_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!((specs[0].name.as_str(), &specs[0].compute, specs[0].max_sandboxes), ("default", &c.compute, None));
    let docker = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\n");
    assert_eq!(parse(&docker).unwrap().target_specs()[0].name, DEFAULT_TARGET);
}

#[test]
fn targets_are_listed_by_priority_then_name() {
    let src = format!(
        "{SAMPLE}\n[targets.sprites]\nkind = \"sprites\"\npriority = 2\nmax_sandboxes = 4\n\n\
         [targets.local]\nkind = \"docker\"\npriority = 1\nmax_sandboxes = 2\nmemory_mib = 4096\n\n\
         [targets.also-local]\nkind = \"docker\"\npriority = 1\n"
    );
    let specs = parse(&src).unwrap().target_specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["also-local", "local", "sprites"]);
    assert_eq!(specs[1].max_sandboxes, Some(2));
    assert_eq!(
        specs[1].compute,
        ComputeConfig::Docker {
            image: None,
            network: None,
            runtime: None,
            memory_mib: Some(4096),
            min_free_bytes: None,
            concurrent_checkpoints: None
        }
    );
    assert_eq!((specs[2].compute.clone(), specs[2].max_sandboxes), (ComputeConfig::Sprites, Some(4)));
}

#[test]
fn a_spend_cap_defaults_to_flys_rates() {
    let src = format!(
        "{SAMPLE}\n[targets.sprites]\nkind = \"sprites\"\ndaily_usd = 5\n\n\
         [targets.dear]\nkind = \"sprites\"\ndaily_usd = 2.5\nusd_per_vcpu_hour = 0.05\nusd_per_gb_hour = 0.03\n"
    );
    let specs = parse(&src).unwrap().target_specs();
    let (dear, sprites) = (specs[0].spend.unwrap(), specs[1].spend.unwrap());
    assert_eq!(
        (sprites.daily, sprites.per_vcpu_hour, sprites.per_gb_hour),
        (crate::Usd::dollars(5.0), crate::Spend::VCPU_HOUR, crate::Spend::GB_HOUR)
    );
    // 8 vCPUs and 4 GB: about $0.40 an hour at Fly's rates.
    assert_eq!(sprites.hourly(), crate::Usd::micros(395_500));
    assert_eq!((dear.daily, dear.hourly()), (crate::Usd::dollars(2.5), crate::Usd::micros(520_000)));
    let plain = format!("{SAMPLE}\n[targets.a]\nkind = \"docker\"\n");
    assert!(parse(&plain).unwrap().target_specs()[0].spend.is_none());
}

#[test]
fn targets_and_compute_together_are_refused() {
    let src = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\n\n[targets.local]\nkind = \"docker\"\n");
    let Err(Error::Config(m)) = parse(&src) else { panic!("expected a config error") };
    assert!(m.contains("[factory.compute]") && m.contains("[factory.targets]"), "{m}");
}

#[test]
fn bad_targets_are_refused() {
    for (bad, says) in [
        ("[targets.local]\nkind = \"vm\"", "unknown kind"),
        ("[targets.local]\nkind = \"sprites\"\nimage = \"x\"", "image"),
        ("[targets.local]\nkind = \"docker\"\nimgae = \"x\"", "imgae"),
        ("[targets.local]\nkind = \"docker\"\nmax_sandboxes = 0", "max_sandboxes"),
        ("[targets.local]\nkind = \"docker\"\ndaily_usd = 0", "daily_usd"),
        ("[targets.local]\nkind = \"docker\"\nusd_per_gb_hour = 0.02", "daily_usd"),
        ("[targets.Local]\nkind = \"docker\"", "name"),
        ("[targets.default]\nkind = \"docker\"", "default"),
        ("[targets.a_b]\nkind = \"docker\"", "name"),
    ] {
        let Err(Error::Config(m)) = parse(&format!("{SAMPLE}\n{bad}\n")) else {
            panic!("{bad}: expected a config error")
        };
        assert!(m.contains(says), "{bad}: {m}");
    }
}

#[test]
fn compute_defaults_to_sprites() {
    assert_eq!(parse(SAMPLE).unwrap().compute, ComputeConfig::Sprites);
    let docker = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nruntime = \"runsc\"\n");
    assert_eq!(
        parse(&docker).unwrap().compute,
        ComputeConfig::Docker {
            image: None,
            network: None,
            runtime: Some("runsc".into()),
            memory_mib: None,
            min_free_bytes: None,
            concurrent_checkpoints: None
        }
    );
    let sized = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nmemory_mib = 1536\n");
    assert_eq!(
        parse(&sized).unwrap().compute,
        ComputeConfig::Docker {
            image: None,
            network: None,
            runtime: None,
            memory_mib: Some(1536),
            min_free_bytes: None,
            concurrent_checkpoints: None
        }
    );
    for mib in [0, 511] {
        let small = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nmemory_mib = {mib}\n");
        let Err(Error::Config(m)) = parse(&small) else { panic!("{mib}: expected a config error") };
        assert!(m.contains("memory_mib") && m.contains("512"), "{m}");
        let target = format!("{SAMPLE}\n[targets.t]\nkind = \"docker\"\nmemory_mib = {mib}\n");
        let Err(Error::Config(m)) = parse(&target) else { panic!("{mib}: expected a config error") };
        assert!(m.contains("memory_mib") && m.contains("512"), "{m}");
    }
    let floor = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nmin_free_bytes = 21474836480\n");
    let ComputeConfig::Docker { min_free_bytes, .. } = parse(&floor).unwrap().compute else { panic!("not docker") };
    assert_eq!(min_free_bytes, Some(20 << 30));
    let pool = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nconcurrent_checkpoints = 4\n");
    let ComputeConfig::Docker { concurrent_checkpoints, .. } = parse(&pool).unwrap().compute else {
        panic!("not docker")
    };
    assert_eq!(concurrent_checkpoints, Some(4));
    for table in ["compute", "targets.t"] {
        let zero = format!("{SAMPLE}\n[{table}]\nkind = \"docker\"\nconcurrent_checkpoints = 0\n");
        let Err(Error::Config(m)) = parse(&zero) else { panic!("{table}: expected a config error") };
        assert!(m.contains("concurrent_checkpoints"), "{m}");
    }
    let sprites = format!("{SAMPLE}\n[targets.t]\nkind = \"sprites\"\nmin_free_bytes = 1\n");
    assert!(matches!(parse(&sprites), Err(Error::Config(_))));
    let least = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nmemory_mib = 512\n");
    assert!(parse(&least).is_ok());
    let bad = format!("{SAMPLE}\n[compute]\nkind = \"docker\"\nimgae = \"x\"\n");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
}

#[test]
fn daemon_log_is_optional() {
    assert_eq!(parse(SAMPLE).unwrap().daemon, DaemonConfig::default());
    let with = format!("{SAMPLE}\n[daemon]\nlog = \"info,iroh=debug\"\n");
    assert_eq!(parse(&with).unwrap().daemon.log.as_deref(), Some("info,iroh=debug"));
    let bad = format!("{SAMPLE}\n[daemon]\nlogg = \"x\"\n");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
    let typo = format!("{SAMPLE}\n[daemon]\nlog = \"info,iroh=bogus\"\n");
    let Err(Error::Config(msg)) = parse(&typo) else { panic!("a bad filter should be refused") };
    assert!(msg.contains("[factory.daemon] log"), "{msg}");
}

#[test]
fn worker_profile_must_have_worker_role() {
    let bad = SAMPLE.replace("[repo]\n", "[repo]\nworker = \"reviewer\"\n");
    assert!(matches!(parse(&bad), Err(Error::Config(_))));
}

#[test]
fn the_repo_file_shares_project_and_defaults_owner_and_name_from_the_remote() {
    let file = format!("agent = \"claude\"\nproject = \"TRAIL\"\n{}", factory_table(SAMPLE));
    let file = file.replace("owner = \"lanesawyer\"\nname = \"trailhead\"\n", "");
    let remote = Repo { owner: "o".into(), name: "n".into() };
    let c = FactoryConfig::from_repo_file(&file, Some(remote.clone())).unwrap();
    assert_eq!((c.repo.project.as_str(), c.repo.repo()), ("TRAIL", remote.clone()));
    assert!(matches!(FactoryConfig::from_repo_file(&file, None), Err(Error::Config(_))));

    let err = |src: &str| match FactoryConfig::from_repo_file(src, Some(remote.clone())) {
        Err(Error::Config(m)) => m,
        other => panic!("expected a config error, got {other:?}"),
    };
    let no_project = file.replace("project = \"TRAIL\"\n", "");
    assert!(err(&no_project).contains("project"), "{}", err(&no_project));
    let repeated = file.replace("[factory.repo]\n", "[factory.repo]\nproject = \"TRAIL\"\n");
    assert!(err(&repeated).contains("project"), "{}", err(&repeated));
    let typo = file.replace("agent =", "agnt =");
    assert!(err(&typo).contains("agnt"), "{}", err(&typo));
}

/// `src` (a bare factory table) with every table header under `[factory.`.
fn factory_table(src: &str) -> String {
    let src = src.replace("\n[", "\n[factory.");
    match src.strip_prefix('[') {
        Some(rest) => format!("[factory.{rest}"),
        None => src,
    }
}
