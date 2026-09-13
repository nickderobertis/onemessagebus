# onemessagebus-agent (the profile)

`tests/recorded/` are real streams each producer wrote, byte-identical. Never
edit one; a fixture that needs a different shape is a new file with its
provenance in the README beside it. `tests/golden/` are `onepipeline`'s golden
documents, copied unchanged.

`schemas/` holds the reply envelope as JSON Schema because the profile owns the
wire shape while `onepipeline` owns each command's meaning; a new version is a
new file registered beside the old, never an edit of one.
