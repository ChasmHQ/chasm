# Chasm AI skill

The maintained skill consists of [SKILL.md](SKILL.md), [the native workspace reference](references/native-workspace.md), and [offline review cases](evals/native-workspace.md). Use it from a project skill directory or install those files with the same relative paths under `~/.agents/skills/chasm-bugbounty/`. No token, listener, global Python installation or running app is needed to read the skill.

When updating a personal installation, compare the two trees first, preserve personal changes, and do not replace unrelated helpers. Check for a higher-priority `.zcode/skills/chasm-bugbounty/` override before attributing behavior to this copy. Depending on the client, a new session may be needed for discovery.

Older local installations may also contain a November 13 schema snapshot, example corpus, validator and evaluation record. They describe that capture, not the current native build. They are not required by the maintained skill. Do not use their age or a passing snapshot validator as evidence of live API behavior. Use the running catalog/schema for current capabilities.
