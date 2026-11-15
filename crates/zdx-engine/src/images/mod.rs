//! Shared image utilities.

pub mod path_mime;

pub use zdx_tools::image_downscale::{
    MAX_PROVIDER_IMAGE_EDGE, downscale_for_provider, downscale_to_fit,
};
