//! Against the real Sprites API. Creates and destroys one small Sprite.
//!
//! `SPRITES_TOKEN=... cargo test -p trailhead-compute-sprites --test live -- --ignored`

use trailhead_compute_sprites::SpritesCompute;
use trailhead_factory::{Compute, Egress, IROH_DOMAINS, Labels, SandboxSpec};

#[tokio::test]
#[ignore = "needs SPRITES_TOKEN and creates a real Sprite"]
async fn lifecycle_against_sprites() {
    let token = std::env::var("SPRITES_TOKEN").expect("SPRITES_TOKEN");
    let compute = SpritesCompute::new(token).unwrap().with_prefix("th-live-");
    let name = format!("th-live-{}", std::process::id());
    let labels = Labels::from([
        ("factory".into(), "trailhead".into()),
        ("repo".into(), "lanesawyer/trailhead".into()),
        ("role".into(), "worker".into()),
    ]);
    let egress = Egress { domains: IROH_DOMAINS.iter().map(|d| d.to_string()).collect() };
    let spec = SandboxSpec { name: name.clone(), labels: labels.clone(), egress };

    let sandbox = compute.create(&spec).await.unwrap();
    // Spawned so a failed assertion still reaches the destroy below.
    let (c, sandbox_id) = (compute.clone(), sandbox.id.clone());
    let result = async move {
        let compute = c;
        assert_eq!(sandbox.labels, labels, "labels as stored by Sprites");
        assert_eq!(compute.create(&spec).await.unwrap().id, sandbox.id);

        let cp = compute.checkpoint(&sandbox.id, "live test").await.unwrap();
        compute.restore(&sandbox.id, &cp).await.unwrap();

        let mut cut = labels.clone();
        cut.remove("role");
        compute.set_labels(&sandbox.id, &cut).await.unwrap();
        compute.set_egress(&sandbox.id, &Egress::none()).await.unwrap();
        let workers = Labels::from([("role".into(), "worker".into())]);
        assert!(compute.list(&workers).await.unwrap().iter().all(|s| s.name != name), "role label removed");
        let found = compute.list(&Labels::from([("factory".into(), "trailhead".into())])).await.unwrap();
        assert_eq!(found.iter().find(|s| s.name == name).map(|s| &s.labels), Some(&cut));
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&sandbox_id).await.unwrap();
    outcome.unwrap();
}
