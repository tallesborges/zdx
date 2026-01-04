//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (effects, commands).
//! See `docs/plans/tui-feature-slice-migration.md` for migration plan.
//!
//! IMPORTANT: This module must NOT import UiEvent or feature-specific state
//! to avoid circular dependencies.

pub mod commands;
pub mod effects;
