//! Unified error type that aggregates all sub-system errors in the painter pipeline.

use crate::io::project::ProjectError;
use crate::mesh::asset_io::{MeshError, TextureError};
use crate::output::OutputError;

/// Unified error type for the painter pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PainterError {
    #[error("Mesh: {0}")]
    Mesh(#[from] MeshError),
    #[error("Texture: {0}")]
    Texture(#[from] TextureError),
    #[error("Output: {0}")]
    Output(#[from] OutputError),
    #[error("Project: {0}")]
    Project(#[from] ProjectError),
}
