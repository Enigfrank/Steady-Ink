mod control;
mod detector;
mod late_bound;
mod session;
mod simulated_keys;

pub use control::{SlideShowControlAction, SlideShowControlBackend};
pub use detector::{
    ComCandidateDiagnostic, ComCandidateStatus, ComDetector, ComDetectorEvent, ComDiagnostics,
};
pub use session::{PresentationApplication, SlidePage, SlideShowKey, SlideShowSession};
