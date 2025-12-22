// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::prelude::*;
use crate::protocols::wprs::Capabilities;
use crate::protocols::wprs::DisplayConfig;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::SendType;
use crate::protocols::wprs::wayland::SurfaceState;

/// Builds messages to represent a surface snapshot.
///
/// Note: buffer payloads are intentionally not produced here. Frame data is
/// transported via `RawBuffer` messages and associated to surfaces by the
/// transport layer.
pub fn surface_messages(state: SurfaceState) -> Result<Vec<SendType<Request>>> {
    Ok(vec![SendType::Object(Request::Surface(
        super::surface_request_from_state(state),
    ))])
}

pub fn initial_messages(
    capabilities: Capabilities,
    display_config: DisplayConfig,
    surfaces: impl IntoIterator<Item = SurfaceState>,
) -> Result<Vec<SendType<Request>>> {
    let mut out = Vec::new();
    out.push(SendType::Object(Request::Capabilities(capabilities)));
    out.push(SendType::Object(Request::DisplayConfig(display_config)));

    for surface in surfaces {
        out.extend(surface_messages(surface).location(loc!())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::protocols::wprs::ClientId;
    use crate::protocols::wprs::wayland::WlSurfaceId;

    fn dummy_surface_state(id: u64) -> SurfaceState {
        SurfaceState {
            client: ClientId(1),
            id: WlSurfaceId(id),
            buffer: None,
            buffer_update: None,
            role: None,
            buffer_scale: 1,
            buffer_transform: None,
            opaque_region: None,
            input_region: None,
            z_ordered_children: Vec::new(),
            damage: None,
            output_ids: Vec::new(),
            viewport_state: None,
            xdg_surface_state: None,
        }
    }

    #[test]
    fn surface_messages_send_only_surface_commit() {
        let surface = dummy_surface_state(2);
        let msgs = surface_messages(surface).unwrap();
        assert!(matches!(
            msgs.as_slice(),
            [SendType::Object(Request::Surface(_))]
        ));
    }

    #[test]
    fn initial_messages_include_caps_display_and_commits() {
        let s1 = dummy_surface_state(10);
        let s2 = dummy_surface_state(20);

        let msgs = initial_messages(
            Capabilities { xwayland: false },
            DisplayConfig::default(),
            [s1, s2],
        )
        .unwrap();

        assert!(matches!(msgs[0], SendType::Object(Request::Capabilities(_))));
        assert!(matches!(msgs[1], SendType::Object(Request::DisplayConfig(_))));
        assert!(matches!(msgs[2], SendType::Object(Request::Surface(_))));
        assert!(matches!(msgs[3], SendType::Object(Request::Surface(_))));
    }
}

