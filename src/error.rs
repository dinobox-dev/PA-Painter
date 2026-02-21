use std::fmt;

use crate::asset_io::{MeshError, TextureError};
use crate::output::OutputError;
use crate::project::ProjectError;

/// Unified error type for the painter pipeline.
#[derive(Debug)]
pub enum PainterError {
    Mesh(MeshError),
    Texture(TextureError),
    Output(OutputError),
    Project(ProjectError),
}

impl fmt::Display for PainterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PainterError::Mesh(e) => write!(f, "Mesh: {e}"),
            PainterError::Texture(e) => write!(f, "Texture: {e}"),
            PainterError::Output(e) => write!(f, "Output: {e}"),
            PainterError::Project(e) => write!(f, "Project: {e}"),
        }
    }
}

impl std::error::Error for PainterError {}

impl From<MeshError> for PainterError {
    fn from(e: MeshError) -> Self {
        PainterError::Mesh(e)
    }
}

impl From<TextureError> for PainterError {
    fn from(e: TextureError) -> Self {
        PainterError::Texture(e)
    }
}

impl From<OutputError> for PainterError {
    fn from(e: OutputError) -> Self {
        PainterError::Output(e)
    }
}

impl From<ProjectError> for PainterError {
    fn from(e: ProjectError) -> Self {
        PainterError::Project(e)
    }
}
