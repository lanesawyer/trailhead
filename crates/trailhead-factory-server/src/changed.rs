//! Which `[factory]` sections of a repo's file differ from what the server started with: all but
//! `[factory.merge]` are read once, so a change to them waits for a restart.

use trailhead_factory::FactoryConfig;

/// The `[factory]` sections of `now` that differ from the `started` config (whose `[factory.merge]` is blanked, as it
/// is re-read live) and so wait for a restart.
pub(crate) fn changed_sections(started: &FactoryConfig, now: &FactoryConfig) -> Vec<&'static str> {
    // Destructured so that a new section can't be left out of the comparison.
    let FactoryConfig {
        repo,
        stack,
        checks,
        profiles,
        gates,
        grades,
        egress,
        compute,
        targets,
        daemon,
        merge: _,
        shots,
    } = started;
    [
        ("repo", *repo != now.repo),
        ("stack", *stack != now.stack),
        ("checks", *checks != now.checks),
        ("profiles", *profiles != now.profiles),
        ("gates", *gates != now.gates),
        ("grades", *grades != now.grades),
        ("egress", *egress != now.egress),
        ("compute", *compute != now.compute),
        ("targets", *targets != now.targets),
        ("daemon", *daemon != now.daemon),
        ("shots", *shots != now.shots),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect()
}

/// [`changed_sections`] for the log: the sections that wait for a restart, unless that same edit was already `warned`
/// about, so it is logged once, not on every read. Empty when there is nothing new to say.
pub(crate) fn changed_without_restart(
    started: &FactoryConfig,
    now: FactoryConfig,
    warned: &mut Option<FactoryConfig>,
) -> Vec<&'static str> {
    let now = FactoryConfig { merge: Default::default(), ..now };
    let changed = changed_sections(started, &now);
    if changed.is_empty() {
        *warned = None;
        return changed;
    }
    if warned.as_ref() == Some(&now) {
        return Vec::new();
    }
    *warned = Some(now);
    changed
}

/// The baseline [`changed_without_restart`] compares against: `config` as read from the file, with `[factory.merge]` blanked
/// as it is re-read live. Take it before `wire` adds anything at runtime, which a re-read of the file never has.
pub(crate) fn started_baseline(config: &FactoryConfig) -> FactoryConfig {
    FactoryConfig { merge: Default::default(), ..config.clone() }
}
