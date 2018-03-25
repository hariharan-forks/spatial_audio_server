//! A `Soundscape` panel displaying:
//!
//! - Play/Pause toggle for the soundscape.
//! - Groups panel for creating/removing soundscape source groups.

use gui::{collapsible_area, Gui, State};
use gui::{ITEM_HEIGHT, SMALL_FONT_SIZE};
use nannou::ui;
use nannou::ui::prelude::*;
use serde_json;
use soundscape;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

/// GUI state related to the soundscape editor area.
pub struct SoundscapeEditor {
    pub is_open: bool,
    pub groups: HashMap<soundscape::group::Id, soundscape::group::Name>,
    pub next_group_id: soundscape::group::Id,
    pub selected: Option<Selected>,
}

/// JSON friendly representation of the soundscape editor GUI state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stored {
    pub groups: HashMap<soundscape::group::Id, soundscape::group::Name>,
    pub next_group_id: soundscape::group::Id,
}

/// The currently selected group.
pub struct Selected {
    name: String,
    id: soundscape::group::Id,
}

impl Stored {
    /// Load the stored soundscape groups from the given path.
    ///
    /// If the path is invalid or the JSON can't be read, `Stored::default` will be called.
    pub fn load(soundscape_path: &Path) -> Self {
        let stored = File::open(&soundscape_path)
            .ok()
            .and_then(|f| serde_json::from_reader(f).ok())
            .unwrap_or_else(Stored::default);
        stored
    }
}

/// Sets all widgets in the soundscape area and returns the `Id` of the last area.
pub fn set(last_area_id: widget::Id, gui: &mut Gui) -> widget::Id {
    let &mut Gui {
        ref mut ui,
        ref ids,
        channels,
        state:
            &mut State {
                ref mut soundscape_editor,
                ..
            },
        ..
    } = gui;

    // Constants to use as widget heights.
    const PAD: Scalar = 6.0;
    const IS_PLAYING_H: Scalar = ITEM_HEIGHT;
    const PLUS_GROUP_H: Scalar = ITEM_HEIGHT;
    const GROUP_LIST_MAX_H: Scalar = ITEM_HEIGHT * 5.0;
    const TEXT_BOX_H: Scalar = ITEM_HEIGHT;
    const TITLE_H: Scalar = SMALL_FONT_SIZE as Scalar * 1.333;
    const GROUP_CANVAS_H: Scalar = PAD + TITLE_H + PAD + PLUS_GROUP_H + GROUP_LIST_MAX_H + PAD;
    const SELECTED_CANVAS_H: Scalar = PAD + TITLE_H + PAD * 2.0 + TEXT_BOX_H + PAD;
    let soundscape_editor_canvas_h = PAD + IS_PLAYING_H + PAD + GROUP_CANVAS_H + PAD + SELECTED_CANVAS_H + PAD;

    // The collapsible area.
    let is_open = soundscape_editor.is_open;
    let (area, event) = collapsible_area(is_open, "Soundscape Editor", ids.side_menu)
        .align_middle_x_of(ids.side_menu)
        .down_from(last_area_id, 0.0)
        .set(ids.soundscape_editor, ui);
    if let Some(event) = event {
        soundscape_editor.is_open = event.is_open();
    }

    // If the area is open, get the area.
    let area = match area {
        Some(area) => area,
        None => return ids.soundscape_editor,
    };

    // The canvas on which the soundscape editor will be placed.
    let canvas = widget::Canvas::new()
        .scroll_kids()
        .pad(PAD)
        .h(soundscape_editor_canvas_h);
    area.set(canvas, ui);

    // The toggle for whether or not the soundscape should be playing back.
    let is_playing = channels.soundscape.is_playing();
    let label = match is_playing {
        true => format!(">> PLAYING >>"),
        false => format!("|| PAUSED ||"),
    };
    for new_is_playing in widget::Toggle::new(is_playing)
        .color(color::BLUE)
        .h(ITEM_HEIGHT)
        .mid_top_of(area.id)
        .kid_area_w_of(area.id)
        .label(&label)
        .label_font_size(SMALL_FONT_SIZE)
        .set(ids.soundscape_editor_is_playing, ui)
    {
        if new_is_playing {
            channels.soundscape.play().ok();
        } else {
            channels.soundscape.pause().ok();
        }
    }

    //////////////////
    // GROUP EDITOR //
    //////////////////

    // A canvas on which group selection and editing takes place.
    widget::Canvas::new()
        .parent(area.id)
        .kid_area_w_of(area.id)
        .h(GROUP_CANVAS_H)
        .align_middle_x_of(area.id)
        .down(PAD)
        .pad(PAD)
        .color(color::CHARCOAL)
        .set(ids.soundscape_editor_group_canvas, ui);

    // A title for the groups canvas.
    widget::Text::new("Groups")
        .top_left_of(ids.soundscape_editor_group_canvas)
        .font_size(SMALL_FONT_SIZE)
        .set(ids.soundscape_editor_group_text, ui);

    // A button for adding new groups.
    for _click in widget::Button::new()
        .label("+")
        .kid_area_w_of(ids.soundscape_editor_group_canvas)
        .h(PLUS_GROUP_H)
        .align_middle_x_of(ids.soundscape_editor_group_canvas)
        .down(PAD * 2.0)
        .set(ids.soundscape_editor_group_add, ui)
    {
        // Add a new group.
        let id = soundscape_editor.next_group_id;
        let next_id = id.0.checked_add(1).expect("the next group `Id` would overflow");
        soundscape_editor.next_group_id = soundscape::group::Id(next_id);
        let name = "<unnamed>".to_string();
        soundscape_editor.groups.insert(id, soundscape::group::Name(name.clone()));
        soundscape_editor.selected = Some(Selected { id, name });
    }

    // If there are no groups, display some text for adding a group.
    if soundscape_editor.groups.is_empty() {
        widget::Text::new("Add a group with the \"+\" button above!")
            .font_size(SMALL_FONT_SIZE)
            .align_middle_x_of(ids.soundscape_editor_group_canvas)
            .down(PAD + ITEM_HEIGHT)
            .set(ids.soundscape_editor_group_none, ui);
        return area.id;
    }

    // Otherwise display the list of all groups that currently exist.
    //
    // First, collect all groups into alphabetical order.
    let mut groups_vec: Vec<_> = soundscape_editor
        .groups
        .iter()
        .map(|(&id, name)| (id, name.0.clone()))
        .collect();
    groups_vec.sort_by(|a, b| a.1.cmp(&b.1));

    // The list widget.listing all groups in alphabetical order.
    let num_groups = groups_vec.len();
    let (mut events, scrollbar) = widget::ListSelect::single(num_groups)
        .down(0.0)
        .flow_down()
        .item_size(ITEM_HEIGHT)
        .h(GROUP_LIST_MAX_H)
        .kid_area_w_of(ids.soundscape_editor_group_canvas)
        .scrollbar_next_to()
        .set(ids.soundscape_editor_group_list, ui);

    // The index of the currently selected group within the group vec.
    let selected_index = soundscape_editor
        .selected
        .as_ref()
        .and_then(|s| groups_vec.iter().position(|&(id, _)| id == s.id));

    // Track whether or not an item was removed.
    let mut maybe_remove_index = None;
    while let Some(event) = events.next(ui, |i| Some(i) == selected_index) {
        use self::ui::widget::list_select::Event;
        match event {
            // Instantiate the widget for this item.
            Event::Item(item) => {
                let is_selected = selected_index == Some(item.i);

                // Blue if selected, gray otherwise.
                let color = if is_selected {
                    color::BLUE
                } else {
                    color::DARK_CHARCOAL
                };

                // Use the name as the label.
                let label = &groups_vec[item.i].1;

                // Use a button widget for each item.
                let label_x = position::Relative::Place(position::Place::Start(Some(10.0)));
                let button = widget::Button::new()
                    .label(&label)
                    .label_font_size(SMALL_FONT_SIZE)
                    .label_x(label_x)
                    .color(color);
                item.set(button, ui);

                // If the button or any of its children are capturing the mouse, display
                // the `remove` button.
                let show_remove_button = ui.global_input()
                    .current
                    .widget_capturing_mouse
                    .map(|id| {
                        id == item.widget_id
                            || ui.widget_graph()
                                .does_recursive_depth_edge_exist(item.widget_id, id)
                    })
                    .unwrap_or(false);

                if !show_remove_button {
                    continue;
                }

                if widget::Button::new()
                    .label("X")
                    .label_font_size(SMALL_FONT_SIZE)
                    .color(color::DARK_RED.alpha(0.5))
                    .w_h(ITEM_HEIGHT, ITEM_HEIGHT)
                    .align_right_of(item.widget_id)
                    .align_middle_y_of(item.widget_id)
                    .parent(item.widget_id)
                    .set(ids.soundscape_editor_group_remove, ui)
                    .was_clicked()
                {
                    maybe_remove_index = Some(item.i);
                }
            },

            // Update the selected source.
            Event::Selection(idx) => {
                soundscape_editor.selected = {
                    let (id, ref name) = groups_vec[idx];
                    Some(Selected { id, name: name.clone() })
                };
            }

            _ => (),
        }
    }

    // The scrollbar for the list.
    if let Some(s) = scrollbar {
        s.set(ui);
    }

    // Remove a group if necessary.
    if let Some(i) = maybe_remove_index {
        let (id, _) = groups_vec.remove(i);

        // Unselect the removed group.
        if soundscape_editor.selected.as_ref().map(|s| s.id) == Some(id) {
            soundscape_editor.selected = None;
        }

        // Remove the local copy from the map.
        soundscape_editor.groups.remove(&id);

        // TODO: Remove this group from any sources on the soundscape thread.
    }

    ////////////////////
    // SELECTED GROUP //
    ////////////////////

    // Only continue if there is some selected group.
    let SoundscapeEditor {
        ref mut groups,
        ref mut selected,
        ..
    } = *soundscape_editor;
    let selected = match selected.as_mut() {
        Some(selected) => selected,
        None => return area.id,
    };

    // A canvas for parameters specific to the selected group.
    widget::Canvas::new()
        .parent(area.id)
        .kid_area_w_of(area.id)
        .h(SELECTED_CANVAS_H)
        .align_middle_x_of(area.id)
        .down_from(ids.soundscape_editor_group_canvas, PAD)
        .pad(PAD)
        .color(color::CHARCOAL)
        .set(ids.soundscape_editor_selected_canvas, ui);

    // A title indicating that the following parameters are for the selected group.
    widget::Text::new("Selected Group")
        .top_left_of(ids.soundscape_editor_selected_canvas)
        .font_size(SMALL_FONT_SIZE)
        .set(ids.soundscape_editor_selected_text, ui);

    // Display a textbox for editing the name.
    for event in widget::TextBox::new(&selected.name)
        .middle_of(ids.soundscape_editor_selected_canvas)
        .down(PAD * 2.0)
        .h(ITEM_HEIGHT)
        .kid_area_w_of(ids.soundscape_editor_selected_canvas)
        .font_size(SMALL_FONT_SIZE)
        .color(color::BLACK)
        .set(ids.soundscape_editor_selected_name, ui)
    {
        use self::ui::widget::text_box::Event;
        match event {
            // When typing generally, only update the temp selected name.
            Event::Update(new_name) => {
                selected.name = new_name;
            },
            // Only when enter is pressed do we update the actual name.
            Event::Enter => {
                if let Some(name) = groups.get_mut(&selected.id) {
                    name.0 = selected.name.clone();
                }
            },
        }
    }

    area.id
}
