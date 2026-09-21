# onemessagebus-agent (the profile)

`tests/recorded/` are real streams each producer wrote, byte-identical. Never
edit one; a fixture that needs a different shape is a new file with its
provenance in the README beside it. `tests/golden/` are golden documents copied
unchanged from the release that wrote them.

The profile is the agent stack's shared vocabulary — the event envelope, the
note, the registry — and nothing else. A protocol one program owns (its
records, its queues and their layout) is that program's to publish as a schema
bundle a configuration links, never a module here.
