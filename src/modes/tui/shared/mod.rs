//! Shared leaf types for TUI features.
//!
//! Contains types with no feature dependencies (effects, commands).
//! These types are shared across all feature modules.
//!
//! IMPORTANT: This module must NOT import UiEvent or feature-specific state
//! to avoid circular dependencies.

pub mod commands;
pub mod effects;
