//! Go's `servers/chipper/options.go`: the options a [`Server`] is built from.
//!
//! [`Server`]: crate::chipper::Server

use std::sync::Arc;

use crate::vtt::{IntentGraphProcessor, IntentProcessor, KgProcessor};

/// Go's `options`, which its variadic `Option` functions mutate. Each
/// processor is optional here, and an unset one answers `Unimplemented`
/// instead of dereferencing nil.
#[derive(Default)]
pub struct Options {
    pub(crate) intent: Option<Arc<dyn IntentProcessor>>,
    pub(crate) kg: Option<Arc<dyn KgProcessor>>,
    pub(crate) intent_graph: Option<Arc<dyn IntentGraphProcessor>>,
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    // TODO(M2): WithLogger(log.Logger), a hugh logger `Server` never reads.

    /// WithIntentProcessor sets the intent processor
    pub fn with_intent_processor(mut self, s: Arc<dyn IntentProcessor>) -> Self {
        self.intent = Some(s);
        self
    }

    /// WithKnowledgeGraphProcessor sets the knowledge graph processor
    pub fn with_knowledge_graph_processor(mut self, s: Arc<dyn KgProcessor>) -> Self {
        self.kg = Some(s);
        self
    }

    /// WithIntentGraphProcessor sets the intent graph processor
    pub fn with_intent_graph_processor(mut self, s: Arc<dyn IntentGraphProcessor>) -> Self {
        self.intent_graph = Some(s);
        self
    }
}
