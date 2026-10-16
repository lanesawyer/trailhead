mod common;

use common::Instance;

#[test]
fn agents_create_their_own_sub_agents_from_the_cli() {
    let th = Instance::new();
    assert!(th.stdout(&["agent", "add", "boss"]).contains("--as boss"));
    let out = th.stdout(&["--as", "boss", "agent", "add", "kid"]);
    assert!(out.starts_with("agent boss/kid"), "{out}");
    assert!(th.stdout(&["--as", "boss/kid", "whoami"]).starts_with("agent boss/kid acting for"));
    assert!(th.stdout(&["--as", "boss/kid", "agent", "add", "notes"]).starts_with("agent boss/kid/notes"));

    // Revoking the parent cuts off everything under it.
    assert!(th.stdout(&["agent", "revoke", "boss"]).contains("revoked boss"));
    let out = th.run(&["--as", "boss/kid", "whoami"]);
    assert!(!out.status.success());
    assert!(th.stdout(&["agent", "ls"]).contains("boss/kid/notes  (revoked)"));
}
