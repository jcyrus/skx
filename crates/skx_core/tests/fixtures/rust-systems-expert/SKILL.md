---
name: rust-systems-expert
description: Deep systems architectural conventions, memory layout, and concurrency patterns
version: 1.0.0
triggers:
  - "*.rs"
  - "Cargo.toml"
targets:
  antigravity:
    scope: workspace
    auto_activate: true
  claude_code:
    enabled: true
  cursor:
    glob: "**/*.rs"
mcp_dependencies:
  - name: rust-analyzer-mcp
    command: rust-analyzer-mcp
    args: ["--stdio"]
---

# Rust Systems Engineering Instructions

- Prefer zero-cost abstractions and enforce explicit lifetime annotations where ambiguity arises.
- Structure error types using `thiserror` for internal libraries and `anyhow` for application boundaries.
