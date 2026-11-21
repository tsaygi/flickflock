//! Fallback for operating systems without a supported system codec.

use std::path::Path;

use crate::{NoIntegration, PlatformDecoder, PlatformError, PlatformIntegration, RawImage};

struct NoDecoder;

impl PlatformDecoder for NoDecoder {
    fn name(&self) -> &'static str {
        "none"
    }

    fn decode(&self, _path: &Path) -> Result<RawImage, PlatformError> {
        Err(PlatformError::Unsupported(
            "HEIC/HEIF images are not supported on this operating system".into(),
        ))
    }
}

pub(crate) fn decoder() -> &'static dyn PlatformDecoder {
    &NoDecoder
}

pub(crate) fn integration() -> &'static dyn PlatformIntegration {
    &NoIntegration
}
