---
applyTo: "crates/**/*.rs"
---
Prefer explicit error types and typed ports. Do not use unwrap/expect in production code without a reason. Preserve existing serde wire forms unless a versioned migration is provided. Add tests for every behavioral change. Keep database implementations behind traits.
