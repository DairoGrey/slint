// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

#pragma once

#include "private/slint_string.h"

#include <cstdint>
#include <vector>

namespace slint {

/// A bounded primitive in a renderer-backed native surface.
struct NativeSurfaceCommand {
    enum class Kind : std::uint8_t { FillRect, Text, Line };

    Kind kind = Kind::FillRect;
    float x = 0.f;
    float y = 0.f;
    float width = 0.f;
    float height = 0.f;
    std::uint32_t color_argb = 0;
    SharedString text;
    SharedString font_family;
    float font_size = 0.f;
    std::int32_t font_weight = 400;
};

/// Immutable producer-side frame. Publishing copies its commands into Slint's
/// UI-thread-local registry; callers can immediately reuse or discard it.
class NativeSurfaceFrame {
public:
    explicit NativeSurfaceFrame(std::uint64_t generation = 0) : generation_(generation) { }

    void set_generation(std::uint64_t generation) { generation_ = generation; }
    [[nodiscard]] std::uint64_t generation() const { return generation_; }
    [[nodiscard]] std::vector<NativeSurfaceCommand> &commands() { return commands_; }
    [[nodiscard]] const std::vector<NativeSurfaceCommand> &commands() const { return commands_; }

private:
    std::uint64_t generation_ = 0;
    std::vector<NativeSurfaceCommand> commands_;
    friend class NativeSurfaceRegistry;
};

/// Public bridge between a C++ host and `NativeSurfaceItem`.
///
/// Publish on Slint's event-loop thread. The registry deliberately carries no
/// renderer objects and is usable by generated Slint components only through
/// their integer `surface-id` property.
class NativeSurfaceRegistry {
public:
    static void publish(std::int32_t surface_id, const NativeSurfaceFrame &frame)
    {
        std::vector<cbindgen_private::NativeSurfaceCommandData> commands;
        commands.reserve(frame.commands_.size());
        for (const auto &command : frame.commands_) {
            commands.push_back({
                    .kind = static_cast<std::uint8_t>(command.kind),
                    .x = command.x,
                    .y = command.y,
                    .width = command.width,
                    .height = command.height,
                    .color_argb = command.color_argb,
                    .text = reinterpret_cast<const std::uint8_t *>(command.text.data()),
                    .text_len = command.text.size(),
                    .font_family = reinterpret_cast<const std::uint8_t *>(command.font_family.data()),
                    .font_family_len = command.font_family.size(),
                    .font_size = command.font_size,
                    .font_weight = command.font_weight,
            });
        }
        cbindgen_private::slint_native_surface_publish(
                surface_id, frame.generation_, commands.data(), commands.size());
    }

    static void clear(std::int32_t surface_id)
    {
        cbindgen_private::slint_native_surface_clear(surface_id);
    }
};

} // namespace slint
