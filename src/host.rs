// The plugin's exports to zellij — `load`, `update`, `pipe`, `render`, `plugin_version` — written
// out instead of `register_plugin!(State)`.
//
// This is the `register_plugin!` macro of `zellij-tile` 0.44.3, line for line, with ONE change:
// `update` decodes the event through `keys::event_from_protobuf`, which reads a keystroke's
// character whole. The macro's `protobuf_event.try_into()` cuts it to a byte (see `keys.rs`), and a
// macro cannot be half-used — so it is not used, and the plugin's entry points are here.
//
// **When `zellij-tile` is upgraded, compare this file with the new macro** (`src/lib.rs` of the
// crate) and carry over whatever changed. The test `keys::the_library_still_truncates_the_same_key`
// says when the workaround itself can go.

use crate::keys;
use crate::State;
use std::cell::RefCell;
use std::collections::BTreeMap;
use zellij_tile::prelude::*;
use zellij_tile::shim::plugin_api::action::ProtobufPluginConfiguration;
use zellij_tile::shim::plugin_api::event::ProtobufEvent;
use zellij_tile::shim::plugin_api::pipe_message::ProtobufPipeMessage;
use zellij_tile::shim::prost::Message;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[no_mangle]
fn load() {
    STATE.with(|state| {
        let protobuf_bytes: Vec<u8> = object_from_stdin().unwrap();
        let protobuf_configuration =
            ProtobufPluginConfiguration::decode(protobuf_bytes.as_slice()).unwrap();
        let configuration: BTreeMap<String, String> =
            BTreeMap::try_from(&protobuf_configuration).unwrap();
        state.borrow_mut().load(configuration);
    });
}

#[no_mangle]
pub fn update() -> bool {
    STATE.with(|state| {
        let protobuf_bytes: Vec<u8> = object_from_stdin().unwrap();
        let protobuf_event = ProtobufEvent::decode(protobuf_bytes.as_slice()).unwrap();
        // The one line that differs from the macro.
        let event = keys::event_from_protobuf(protobuf_event).unwrap();
        state.borrow_mut().update(event)
    })
}

#[no_mangle]
pub fn pipe() -> bool {
    STATE.with(|state| {
        let protobuf_bytes: Vec<u8> = object_from_stdin().unwrap();
        let protobuf_pipe_message = ProtobufPipeMessage::decode(protobuf_bytes.as_slice()).unwrap();
        let pipe_message: PipeMessage = protobuf_pipe_message.try_into().unwrap();
        state.borrow_mut().pipe(pipe_message)
    })
}

#[no_mangle]
pub fn render(rows: i32, cols: i32) {
    STATE.with(|state| {
        state.borrow_mut().render(rows as usize, cols as usize);
    });
}

#[no_mangle]
pub fn plugin_version() {
    println!("{}", VERSION);
}
