# Changelog

All notable changes to Trailhead are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Features

- **core:** phase 1 domain core ([0cbf554](https://github.com/lanesawyer/trailhead/commit/0cbf554a01d48057fc1bb536c5eeb531dcbcdb76))
- REST server and CLI dogfood slice ([60085ca](https://github.com/lanesawyer/trailhead/commit/60085cab94e13a1fafd789ceca6e9f5b8cdf197c))
- **cli:** manage the local server and restart it after upgrades ([fd11ece](https://github.com/lanesawyer/trailhead/commit/fd11ece01da1d1e5a1f8c159c0335e251aeab446))
- **server:** MCP endpoint at /mcp ([ec8c822](https://github.com/lanesawyer/trailhead/commit/ec8c8223e511e37db240d678db010420e60b3b02))
- SSE event stream and trailhead watch ([95fa910](https://github.com/lanesawyer/trailhead/commit/95fa9107a4aea3822b32b592e3eaed713b0a7ad9))
- **cli:** trailhead mcp stdio bridge and project .mcp.json ([3525f6d](https://github.com/lanesawyer/trailhead/commit/3525f6d6e60be31ee5481126e687644749f3e4d0))
- **cli:** record due_tz and handle DST in due dates ([a7c4b44](https://github.com/lanesawyer/trailhead/commit/a7c4b447efce96ff4e4fd6f717bd5b1ce98cb0e4))
- **core:** name the holder in claim_held errors ([0b3df7b](https://github.com/lanesawyer/trailhead/commit/0b3df7b1e8965033eeb268c5e0ce4cfa4a8c7bf0))
- **server:** Idempotency-Key on /v1 writes ([0d13f69](https://github.com/lanesawyer/trailhead/commit/0d13f6945ff32e42b6514fc4fdd1e4c160f51c7e))
- **server:** MCP undo_last for an agent's own last change ([af0724d](https://github.com/lanesawyer/trailhead/commit/af0724de5a2a1bab461f4383b92d55fdc18caea4))
- **server:** OpenAPI document at /v1/openapi.json ([471f84e](https://github.com/lanesawyer/trailhead/commit/471f84ea359e037248e641c7cde6151edaa99454))
- **core:** project-scoped tokens and rate limit fields ([60a4c6c](https://github.com/lanesawyer/trailhead/commit/60a4c6cedec5eb82876650b1b4b3ed4c34191a86))
- **server:** per-token rate limits, POST /v1/tokens and scoped agent tokens ([48b47d8](https://github.com/lanesawyer/trailhead/commit/48b47d8eb8f532aaf612055bda0b43de95ab2c5c))
- **cli:** show time zones and relative lease expiry ([0fcecd6](https://github.com/lanesawyer/trailhead/commit/0fcecd6dd170fb1807e7868068c53af7b976540f))
- **core:** reject unknown due_tz zones ([7b7b2ee](https://github.com/lanesawyer/trailhead/commit/7b7b2ee7f4eeba9bb55824968dffe901f9d88b19))
- **core:** archive events older than 90 days and enforce the undo window ([2687004](https://github.com/lanesawyer/trailhead/commit/2687004a3ced11b9f98dd8b706a6ed6419ac397f))
- **core:** task.due_soon and task.overdue reminders ([9c8c3d4](https://github.com/lanesawyer/trailhead/commit/9c8c3d47064a6c5d2d0ede20a5106a24cc603689))
- **server:** run due reminders every minute and hide them from undo-last ([572b43e](https://github.com/lanesawyer/trailhead/commit/572b43e89868393578980fd688f0af761977197e))
- **cli:** keyring credentials and per-repo default identity ([e7c6f9c](https://github.com/lanesawyer/trailhead/commit/e7c6f9ce72d25d3ff99e6eb48c3f22c15a7aac6b))
- tell agents when they've lost a claim ([c048f5e](https://github.com/lanesawyer/trailhead/commit/c048f5e57257419235c11b594f360db6bea545da))
- **server:** idempotency keys for MCP create_task and comment ([3a933eb](https://github.com/lanesawyer/trailhead/commit/3a933eb2eac89b8d7642b5c86ae55bb6550b651b))
- keep the local user's credential in the OS keyring ([f801ee1](https://github.com/lanesawyer/trailhead/commit/f801ee1fda406a9f4db519137ee517ba9b81dc6e))
- **server:** MCP update_task takes a handoff ([2e1a4de](https://github.com/lanesawyer/trailhead/commit/2e1a4de2360ced791e11aa60c79027c6171fc5fb))
- outbound webhooks ([a1f2935](https://github.com/lanesawyer/trailhead/commit/a1f293564ada8940323b46290dc9dc371da3ae65))
- **server:** serve llms.txt at /llms.txt ([7de77e4](https://github.com/lanesawyer/trailhead/commit/7de77e4d9474cbf45918bf520066347b2f4f8f0a))
- delegate review approval to agents per project ([c3bcd44](https://github.com/lanesawyer/trailhead/commit/c3bcd44bc5bd772e68c5b9d5f5c5f29f7cca57e4))
- agents create their own sub-agents ([7e07506](https://github.com/lanesawyer/trailhead/commit/7e075066674f05ce1920e01db3087335d9eebfaf))
- **server:** MCP tool safety annotations ([510d770](https://github.com/lanesawyer/trailhead/commit/510d77069ca8a57fe0fa28a2267228a79026512c))

### Bug fixes

- **server:** keep OpenAPI error codes in step after rebase ([fd44704](https://github.com/lanesawyer/trailhead/commit/fd44704583f9b5f6a926c84f6fc90ed9051c6b4c))
- **server:** scope idempotency keys to the token ([987a539](https://github.com/lanesawyer/trailhead/commit/987a53966ba3fd5d552bbadbc4001afbb88a6e31))
- **cli:** only stop a server that serves this CLI's database ([1527f32](https://github.com/lanesawyer/trailhead/commit/1527f32c134a4d41d0624b30f119bf65148f4613))
- **core:** agent names are unique per workspace ([257063b](https://github.com/lanesawyer/trailhead/commit/257063bcf625c10d78da96999e2bbcd65a426cf5))
- **cli:** answer takes a task key ([2fe4039](https://github.com/lanesawyer/trailhead/commit/2fe4039ccf31a0b82a396e2e4dc5c167af9acdee))

### Documentation

- require agents to track work in TRAIL ([351d730](https://github.com/lanesawyer/trailhead/commit/351d730966a9e31fc388ffce434bf99ad098c503))
- **server:** document idempotency keys and rate limits in OpenAPI ([7fc7ae5](https://github.com/lanesawyer/trailhead/commit/7fc7ae556d8ce564bebbb57122bb949f71f922d3))
- portable trailhead agent skill ([1b08094](https://github.com/lanesawyer/trailhead/commit/1b08094fe98bcfc5be0831008a978e6ee4940a4e))
- webhooks in llms.txt, and mark Phase 3 as current ([9dfb0a6](https://github.com/lanesawyer/trailhead/commit/9dfb0a6a9ab95d2e91adee2273d382f62db1e972))
- **server:** make OpenAPI error lists match the handlers ([c0e8435](https://github.com/lanesawyer/trailhead/commit/c0e8435c61414d72382c0101e77f0819e6d88b73))
- OpenClaw integration through MCP and the agent skill ([b485fc9](https://github.com/lanesawyer/trailhead/commit/b485fc97224902602b49c98528532daa4f08de34))

### Testing

- **cli:** clear TRAILHEAD_DB and TRAILHEAD_BIND in the test harness ([a479b56](https://github.com/lanesawyer/trailhead/commit/a479b56e3c6a87f592f2e33663553e96df7a2418))

### Miscellaneous

- load the TRAIL queue at session start ([22834e7](https://github.com/lanesawyer/trailhead/commit/22834e711249f0b10320a987360e788e46bb394d))
- **ci:** run fmt, clippy and tests on GitHub Actions ([f31d6cf](https://github.com/lanesawyer/trailhead/commit/f31d6cf6d669546d97b7357b1bf3fee92992ea8f))
- default this repo's trailhead identity to claude ([29c4c12](https://github.com/lanesawyer/trailhead/commit/29c4c129e3914f010024de516fab1b6cab0ac0a2))


