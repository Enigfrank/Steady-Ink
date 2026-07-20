mod document;
mod model;
mod page_store;
pub(crate) mod renderer;
mod stroke_geometry;

pub use document::InkDocument;
pub use model::{
    CanvasPoint, ClearOperation, DrawStroke, EraseSample, EraseStroke, EraserSize, InkBounds,
    InkColor, InkOperation, InkTool, OperationId, PenWidth,
};
pub use page_store::{PageInkEntry, PageInkStore, PageKey};
pub use renderer::{ActiveInkPreview, InkRenderCache};
