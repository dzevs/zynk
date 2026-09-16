// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::borrow::Cow;

use ratatui::layout::Rect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopupSize {
    Cells(u16),
    Percent(u8),
}

impl PopupSize {
    pub(crate) fn resolve(self, available: u16) -> u16 {
        match self {
            Self::Cells(cells) => cells,
            Self::Percent(percent) => ((available as u32 * percent as u32) / 100) as u16,
        }
    }

    pub(crate) fn parse_cli(value: &str) -> Result<Self, String> {
        if let Some(percent) = value.strip_suffix('%') {
            if percent.starts_with('0') || !percent.bytes().all(|b| b.is_ascii_digit()) {
                return Err("percentage must use canonical digits from 1% to 100%".to_string());
            }
            let percent = percent
                .parse::<u8>()
                .map_err(|_| "must be a number of cells or a percentage like 80%".to_string())?;
            if !(1..=100).contains(&percent) {
                return Err("percentage must be between 1% and 100%".to_string());
            }
            return Ok(Self::Percent(percent));
        }
        value
            .parse::<u16>()
            .map(Self::Cells)
            .map_err(|_| "must be a number of cells or a percentage like 80%".to_string())
    }

    fn parse_percent_string(value: &str) -> Result<Self, String> {
        if value.ends_with('%') {
            return Self::parse_cli(value);
        }
        Err("string sizes must be percentages like 80%; use a number for cells".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PopupResolvedGeometry {
    pub outer: Rect,
    pub inner: Rect,
}

pub(crate) fn resolve_popup_geometry(
    width: Option<PopupSize>,
    height: Option<PopupSize>,
    area: Rect,
) -> Option<PopupResolvedGeometry> {
    area.x.checked_add(area.width)?;
    area.y.checked_add(area.height)?;
    let default_width = area.width.saturating_div(2).max(6);
    let default_height = area.height.saturating_div(2).max(4);
    let outer_width = width
        .map(|width| width.resolve(area.width))
        .unwrap_or(default_width)
        .max(6)
        .min(area.width);
    let outer_height = height
        .map(|height| height.resolve(area.height))
        .unwrap_or(default_height)
        .max(4)
        .min(area.height);
    if outer_width < 6 || outer_height < 4 {
        return None;
    }

    let outer_x = area.x + (area.width.saturating_sub(outer_width)) / 2;
    let outer_y = area.y + (area.height.saturating_sub(outer_height)) / 2;
    let pane_inner_width = outer_width.saturating_sub(2);
    let pane_inner_height = outer_height.saturating_sub(2);
    let terminal_cols = if pane_inner_width <= 4 {
        pane_inner_width
    } else {
        pane_inner_width.saturating_sub(1)
    };
    let inner = Rect::new(
        outer_x.saturating_add(1),
        outer_y.saturating_add(1),
        terminal_cols,
        pane_inner_height,
    );
    Some(PopupResolvedGeometry {
        outer: Rect::new(outer_x, outer_y, outer_width, outer_height),
        inner,
    })
}

impl Serialize for PopupSize {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Cells(cells) => serializer.serialize_u16(*cells),
            Self::Percent(percent) => serializer.serialize_str(&format!("{percent}%")),
        }
    }
}

impl<'de> Deserialize<'de> for PopupSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PopupSizeVisitor;

        impl serde::de::Visitor<'_> for PopupSizeVisitor {
            type Value = PopupSize;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a cell count or percentage string like 80%")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value =
                    u16::try_from(value).map_err(|_| E::custom("cell count must fit in u16"))?;
                Ok(PopupSize::Cells(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = u16::try_from(value)
                    .map_err(|_| E::custom("cell count must be between 0 and 65535"))?;
                Ok(PopupSize::Cells(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                PopupSize::parse_percent_string(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(PopupSizeVisitor)
    }
}

impl schemars::JsonSchema for PopupSize {
    fn schema_name() -> Cow<'static, str> {
        "PopupSize".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "oneOf": [
                {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 65535,
                    "description": "Outer popup size in terminal cells, including the border."
                },
                {
                    "type": "string",
                    "pattern": "^(100|[1-9][0-9]?)%$",
                    "description": "Outer popup size as a percentage of the terminal area, for example 80%."
                }
            ]
        })
    }
}
