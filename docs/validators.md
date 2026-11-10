# Validators

<!-- llmlint: ignore[no_redundant_instruction_pointers] this node is required to replace docs/validators.md with a short pointer to docs/hooks.md saying validators were removed and in which release; the pointer is this document's whole content, so a reader arriving from an old link reaches the migration rather than a dead page. -->
Validators were removed in onemessagebus 0.11.0. Hooks replace them, and
[`docs/hooks.md`](hooks.md) states how to declare a validator's entry as a hook
and what a configuration that still declares `validators:` is refused with.
