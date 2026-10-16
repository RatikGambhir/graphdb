//! Colour assignment for the graph view.
//!
//! Colour is categorical: it encodes an entity's label and nothing else. Eight
//! hues are taken in fixed order and never cycled; a graph with more labels
//! folds the rarest into a neutral "Other" slot rather than reusing a hue.

use std::collections::HashMap;

use gpui::{Hsla, rgb};

use crate::theme::Mode;

/// The overflow bucket's name.
pub const OTHER_LABEL: &str = "Other";

const LIGHT: [u32; 8] = [
    0x2569a8, 0xb75630, 0x32734e, 0xa16c0c, 0xa34069, 0x47721b, 0x6654a5, 0xb0443f,
];
const DARK: [u32; 8] = [
    0x74b9eb, 0xef9c6d, 0x80c7a1, 0xecc376, 0xe792b1, 0x91bd8d, 0xb5a4ec, 0xef8d87,
];
const NEUTRAL_LIGHT: u32 = 0x728075;
const NEUTRAL_DARK: u32 = 0x91a092;

/// The number of labels that get their own hue.
pub const MAX_COLOURED_LABELS: usize = 8;

/// One legend row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegendEntry {
    pub label: String,
    pub count: u64,
    /// `None` for the neutral overflow bucket.
    pub slot: Option<usize>,
}

/// Assigns colour slots to labels: most common first, ties alphabetically.
#[derive(Clone, Debug, Default)]
pub struct LabelPalette {
    slots: HashMap<String, usize>,
    pub legend: Vec<LegendEntry>,
    folded: usize,
}

impl LabelPalette {
    /// Builds the palette from the label of every node.
    pub fn new<'a>(labels: impl IntoIterator<Item = &'a str>) -> Self {
        let mut counts: HashMap<&str, u64> = HashMap::new();
        for label in labels {
            *counts.entry(label).or_default() += 1;
        }
        let mut ordered: Vec<(&str, u64)> = counts.into_iter().collect();
        ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let mut palette = Self::default();
        let mut overflow = 0;
        for (label, count) in ordered {
            if palette.slots.len() < MAX_COLOURED_LABELS {
                let slot = palette.slots.len();
                palette.slots.insert(label.to_owned(), slot);
                palette.legend.push(LegendEntry {
                    label: label.to_owned(),
                    count,
                    slot: Some(slot),
                });
            } else {
                overflow += count;
                palette.folded += 1;
            }
        }
        if overflow > 0 {
            let name = if palette.slots.contains_key(OTHER_LABEL) {
                format!("{OTHER_LABEL} labels")
            } else {
                OTHER_LABEL.to_owned()
            };
            palette.legend.push(LegendEntry {
                label: name,
                count: overflow,
                slot: None,
            });
        }
        palette
    }

    /// Labels that did not get their own hue.
    #[cfg(test)]
    #[must_use]
    pub fn overflow_count(&self) -> usize {
        self.folded
    }

    /// The colour of a label.
    #[must_use]
    pub fn colour(&self, label: &str, mode: Mode) -> Hsla {
        slot_colour(self.slots.get(label).copied(), mode)
    }

    /// `true` when the label has its own hue.
    #[cfg(test)]
    #[must_use]
    pub fn has_slot(&self, label: &str) -> bool {
        self.slots.contains_key(label)
    }
}

/// Colour of a legend slot, or of the neutral bucket.
#[must_use]
pub fn slot_colour(slot: Option<usize>, mode: Mode) -> Hsla {
    let hex = match (slot, mode) {
        (Some(i), Mode::Light) => LIGHT[i % LIGHT.len()],
        (Some(i), Mode::Dark) => DARK[i % DARK.len()],
        (None, Mode::Light) => NEUTRAL_LIGHT,
        (None, Mode::Dark) => NEUTRAL_DARK,
    };
    rgb(hex).into()
}

/// Colours for a bar chart over an ordered label list.
#[must_use]
pub fn ordered_colours(count: usize, mode: Mode) -> Vec<Hsla> {
    (0..count)
        .map(|i| slot_colour((i < MAX_COLOURED_LABELS).then_some(i), mode))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_rare_labels_into_other() {
        let labels: Vec<String> = (0..10)
            .flat_map(|i| std::iter::repeat_n(format!("L{i}"), 10 - i))
            .collect();
        let palette = LabelPalette::new(labels.iter().map(String::as_str));
        assert_eq!(palette.legend.len(), 9);
        assert_eq!(palette.legend[0].label, "L0");
        assert_eq!(palette.legend.last().unwrap().label, OTHER_LABEL);
        assert_eq!(palette.overflow_count(), 2);
        assert!(!palette.has_slot("L9"));
    }
}
