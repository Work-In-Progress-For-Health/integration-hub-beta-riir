//! The plugin list.
//!
//! Implements `spec/integration-hub-tester-component-preview.md` §2.5.

use crate::plugin::Plugin;
use crate::plugins::proms::PromsPlugin;
use crate::plugins::senders::{Hl7SenderAckPlugin, Hl7SenderPlugin, SoapSenderPlugin};
use crate::plugins::servers::{Hl7ServerPlugin, Hl7SoapServerPlugin, RestServerPlugin};
use crate::plugins::transformers::{ChemoPlugin, CoreReferencePlugin, PhwPlugin, PimsPlugin};

/// A named group of plugins, in tab order (§2.5). The group is a display
/// concept only: plugin ids are unique across all groups.
pub struct Category {
    /// The group heading.
    pub name: &'static str,
    /// Its plugins.
    pub plugins: Vec<Box<dyn Plugin>>,
}

/// Every category, in the Python's `PLUGIN_CATEGORIES` order.
#[must_use]
pub fn categories() -> Vec<Category> {
    vec![
        Category {
            name: "Transformers",
            plugins: vec![
                Box::new(PhwPlugin),
                Box::new(ChemoPlugin),
                Box::new(PimsPlugin),
                Box::new(CoreReferencePlugin),
                Box::new(PromsPlugin),
            ],
        },
        Category {
            name: "Servers",
            plugins: vec![
                Box::new(Hl7ServerPlugin),
                Box::new(RestServerPlugin),
                Box::new(Hl7SoapServerPlugin),
            ],
        },
        Category {
            name: "Senders",
            plugins: vec![
                Box::new(Hl7SenderPlugin),
                Box::new(Hl7SenderAckPlugin),
                Box::new(SoapSenderPlugin::queue()),
                Box::new(SoapSenderPlugin::subscription()),
            ],
        },
    ]
}

/// The plugin called `id`, if there is one (exact, case-sensitive match).
///
/// Rebuilds the whole list on each call; cheap, since plugins are unit
/// structs, and it avoids a global.
#[must_use]
pub fn find(id: &str) -> Option<Box<dyn Plugin>> {
    categories()
        .into_iter()
        .flat_map(|category| category.plugins)
        .find(|plugin| plugin.id() == id)
}
