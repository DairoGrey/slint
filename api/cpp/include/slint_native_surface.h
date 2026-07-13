// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

#pragma once

#include "private/slint_string.h"

#include <cstdint>
#include <vector>

namespace slint {

enum class NativeSurfaceHorizontalAlignment : std::uint8_t { Left, Center, Right };
enum class NativeSurfaceVerticalAlignment : std::uint8_t { Top, Center, Bottom };
enum class NativeSurfaceLayer : std::uint8_t { Base = 1, Underlay = 2, Overlay = 4 };

constexpr std::uint8_t operator|(NativeSurfaceLayer left, NativeSurfaceLayer right)
{
    return static_cast<std::uint8_t>(left) | static_cast<std::uint8_t>(right);
}

/// A coloured byte range within a UTF-8 NativeSurfaceCommand::Text payload.
/// Ranges must be ordered, non-overlapping and align with UTF-8 boundaries.
/// The base command colour remains the fallback outside these ranges.
struct NativeSurfaceTextSpan {
    std::uint32_t start_byte = 0;
    std::uint32_t end_byte = 0;
    std::uint32_t color_argb = 0;
};

/// One cluster produced by the renderer's shaping pass. Coordinates are
/// logical and relative to the corresponding text command's origin.
struct NativeSurfaceLayoutCluster {
    std::uint32_t start_byte = 0;
    std::uint32_t end_byte = 0;
    float x = 0.f;
    float width = 0.f;
};

/// Immutable layout data delivered during the native-surface render pass.
/// `clusters` is valid only for the duration of the callback.
struct NativeSurfaceLayoutSnapshot {
    std::uint64_t layout_key = 0;
    float baseline = 0.f;
    float advance = 0.f;
    const NativeSurfaceLayoutCluster *clusters = nullptr;
    std::size_t cluster_count = 0;
};

/// A bounded primitive in a renderer-backed native surface.
struct NativeSurfaceCommand {
    enum class Kind : std::uint8_t { FillRect, Text, Line };

    Kind kind = Kind::FillRect;
    std::uint64_t layout_key = 0;
    float x = 0.f;
    float y = 0.f;
    float width = 0.f;
    float height = 0.f;
    std::uint32_t color_argb = 0;
    SharedString text;
    std::vector<NativeSurfaceTextSpan> text_spans;
    SharedString font_family;
    float font_size = 0.f;
    std::int32_t font_weight = 400;
    NativeSurfaceHorizontalAlignment horizontal_alignment = NativeSurfaceHorizontalAlignment::Left;
    NativeSurfaceVerticalAlignment vertical_alignment = NativeSurfaceVerticalAlignment::Top;
};

/// Immutable producer-side frame. Publishing copies its commands into Slint's
/// UI-thread-local registry; callers can immediately reuse or discard it.
class NativeSurfaceFrame {
public:
    explicit NativeSurfaceFrame(std::uint64_t generation = 0) : generation_(generation) { }

    void set_generation(std::uint64_t generation) { generation_ = generation; }
    [[nodiscard]] std::uint64_t generation() const { return generation_; }
    void set_base_generation(std::uint64_t generation) { base_generation_ = generation; }
    void set_underlay_generation(std::uint64_t generation) { underlay_generation_ = generation; }
    void set_overlay_generation(std::uint64_t generation) { overlay_generation_ = generation; }
    [[nodiscard]] std::uint64_t base_generation() const { return base_generation_; }
    [[nodiscard]] std::uint64_t underlay_generation() const { return underlay_generation_; }
    [[nodiscard]] std::uint64_t overlay_generation() const { return overlay_generation_; }
    [[nodiscard]] std::vector<NativeSurfaceCommand> &commands() { return commands_; }
    [[nodiscard]] const std::vector<NativeSurfaceCommand> &commands() const { return commands_; }
    [[nodiscard]] std::vector<NativeSurfaceCommand> &underlay_commands() { return underlay_commands_; }
    [[nodiscard]] const std::vector<NativeSurfaceCommand> &underlay_commands() const { return underlay_commands_; }
    [[nodiscard]] std::vector<NativeSurfaceCommand> &overlay_commands() { return overlay_commands_; }
    [[nodiscard]] const std::vector<NativeSurfaceCommand> &overlay_commands() const { return overlay_commands_; }

private:
    std::uint64_t generation_ = 0;
    std::uint64_t base_generation_ = 0;
    std::uint64_t underlay_generation_ = 0;
    std::uint64_t overlay_generation_ = 0;
    std::vector<NativeSurfaceCommand> commands_;
    std::vector<NativeSurfaceCommand> underlay_commands_;
    std::vector<NativeSurfaceCommand> overlay_commands_;
    friend class NativeSurfaceRegistry;
};

/// Public bridge between a C++ host and `NativeSurfaceItem`.
///
/// Publish on Slint's event-loop thread. The registry deliberately carries no
/// renderer objects and is usable by generated Slint components only through
/// their integer `surface-id` property.
class NativeSurfaceRegistry {
public:
    using rendered_callback = void (*)(std::int32_t surface_id, std::uint64_t generation, void* user_data);
    using draw_started_callback = void (*)(std::int32_t surface_id, std::uint64_t generation,
                                           std::size_t base_commands, std::size_t underlay_commands,
                                           std::size_t overlay_commands, void* user_data);
    using layout_callback = void (*)(std::int32_t surface_id, std::uint64_t base_generation,
                                     const NativeSurfaceLayoutSnapshot&, void* user_data);

    /// Receives a UI-thread notification when the backend has completed the
    /// native-surface draw pass. This does not claim OS-compositor/vsync
    /// completion, which is intentionally outside this portable API.
    static void set_rendered_callback(rendered_callback callback, void* user_data = nullptr)
    {
        cbindgen_private::slint_native_surface_set_rendered_callback(callback, user_data);
    }

    /// Receives a UI-thread marker immediately before the renderer starts a
    /// native-surface frame. Pair it with set_rendered_callback() to measure
    /// renderer work independently from event-loop wakeup latency.
    static void set_draw_started_callback(draw_started_callback callback, void* user_data = nullptr)
    {
        cbindgen_private::slint_native_surface_set_draw_started_callback(callback, user_data);
    }

    /// Receives cluster geometry produced by the same backend shaping pass
    /// that draws a NativeSurfaceCommand::Text. The callback runs on the UI
    /// thread and must copy data it needs after return.
    static void set_layout_callback(layout_callback callback, void* user_data = nullptr)
    {
        layout_callback_ = callback;
        layout_user_data_ = user_data;
        cbindgen_private::slint_native_surface_set_layout_callback(
            callback ? &NativeSurfaceRegistry::layout_callback_adapter : nullptr, nullptr);
    }

    static void publish(std::int32_t surface_id, const NativeSurfaceFrame &frame)
    {
        const auto encode = [](const std::vector<NativeSurfaceCommand>& source,
                               std::vector<cbindgen_private::NativeSurfaceCommandData>& commands,
                               std::vector<cbindgen_private::NativeSurfaceTextSpanData>& spans) {
            std::size_t span_count = 0;
            for (const auto &command : source) span_count += command.text_spans.size();
            spans.reserve(span_count);
            commands.reserve(source.size());
            for (const auto &command : source) {
            const auto first_span = spans.size();
            for (const auto &span : command.text_spans) {
                spans.push_back({
                    .start_byte = span.start_byte,
                    .end_byte = span.end_byte,
                    .color_argb = span.color_argb,
                });
            }
                commands.push_back({
                    .kind = static_cast<std::uint8_t>(command.kind),
                    .layout_key = command.layout_key,
                    .x = command.x,
                    .y = command.y,
                    .width = command.width,
                    .height = command.height,
                    .color_argb = command.color_argb,
                    .text = reinterpret_cast<const std::uint8_t *>(command.text.data()),
                    .text_len = command.text.size(),
                    .text_spans = command.text_spans.empty() ? nullptr : spans.data() + first_span,
                    .text_span_count = command.text_spans.size(),
                    .font_family = reinterpret_cast<const std::uint8_t *>(command.font_family.data()),
                    .font_family_len = command.font_family.size(),
                    .font_size = command.font_size,
                    .font_weight = command.font_weight,
                    .horizontal_alignment = static_cast<std::uint8_t>(command.horizontal_alignment),
                    .vertical_alignment = static_cast<std::uint8_t>(command.vertical_alignment),
            });
            }
        };
        std::vector<cbindgen_private::NativeSurfaceCommandData> commands;
        std::vector<cbindgen_private::NativeSurfaceTextSpanData> spans;
        std::vector<cbindgen_private::NativeSurfaceCommandData> overlay_commands;
        std::vector<cbindgen_private::NativeSurfaceTextSpanData> overlay_spans;
        std::vector<cbindgen_private::NativeSurfaceCommandData> underlay_commands;
        std::vector<cbindgen_private::NativeSurfaceTextSpanData> underlay_spans;
        encode(frame.commands_, commands, spans);
        encode(frame.underlay_commands_, underlay_commands, underlay_spans);
        encode(frame.overlay_commands_, overlay_commands, overlay_spans);
        cbindgen_private::slint_native_surface_publish_layers(
                surface_id, frame.generation_, frame.base_generation_, frame.underlay_generation_, frame.overlay_generation_,
                commands.data(), commands.size(), underlay_commands.data(), underlay_commands.size(),
                overlay_commands.data(), overlay_commands.size());
    }

    /// Replaces only selected layers. A selected empty vector clears that
    /// layer; all omitted layers retain their registered immutable commands.
    static void publish_delta(std::int32_t surface_id, const NativeSurfaceFrame &frame, std::uint8_t changed_layers)
    {
        const auto encode = [](const std::vector<NativeSurfaceCommand>& source,
                               std::vector<cbindgen_private::NativeSurfaceCommandData>& commands,
                               std::vector<cbindgen_private::NativeSurfaceTextSpanData>& spans) {
            std::size_t span_count = 0;
            for (const auto &command : source) span_count += command.text_spans.size();
            spans.reserve(span_count);
            commands.reserve(source.size());
            for (const auto &command : source) {
                const auto first_span = spans.size();
                for (const auto &span : command.text_spans) {
                    spans.push_back({ .start_byte = span.start_byte, .end_byte = span.end_byte, .color_argb = span.color_argb });
                }
                commands.push_back({
                    .kind = static_cast<std::uint8_t>(command.kind), .layout_key = command.layout_key, .x = command.x, .y = command.y,
                    .width = command.width, .height = command.height, .color_argb = command.color_argb,
                    .text = reinterpret_cast<const std::uint8_t *>(command.text.data()), .text_len = command.text.size(),
                    .text_spans = command.text_spans.empty() ? nullptr : spans.data() + first_span,
                    .text_span_count = command.text_spans.size(),
                    .font_family = reinterpret_cast<const std::uint8_t *>(command.font_family.data()),
                    .font_family_len = command.font_family.size(), .font_size = command.font_size,
                    .font_weight = command.font_weight,
                    .horizontal_alignment = static_cast<std::uint8_t>(command.horizontal_alignment),
                    .vertical_alignment = static_cast<std::uint8_t>(command.vertical_alignment),
                });
            }
        };
        std::vector<cbindgen_private::NativeSurfaceCommandData> base, underlay, overlay;
        std::vector<cbindgen_private::NativeSurfaceTextSpanData> base_spans, underlay_spans, overlay_spans;
        if (changed_layers & static_cast<std::uint8_t>(NativeSurfaceLayer::Base)) encode(frame.commands_, base, base_spans);
        if (changed_layers & static_cast<std::uint8_t>(NativeSurfaceLayer::Underlay)) encode(frame.underlay_commands_, underlay, underlay_spans);
        if (changed_layers & static_cast<std::uint8_t>(NativeSurfaceLayer::Overlay)) encode(frame.overlay_commands_, overlay, overlay_spans);
        cbindgen_private::slint_native_surface_publish_layers_delta(
            surface_id, frame.generation_, frame.base_generation_, frame.underlay_generation_, frame.overlay_generation_, changed_layers,
            base.data(), base.size(), underlay.data(), underlay.size(), overlay.data(), overlay.size());
    }

    static void clear(std::int32_t surface_id)
    {
        cbindgen_private::slint_native_surface_clear(surface_id);
    }

private:
    static void layout_callback_adapter(
        std::int32_t surface_id,
        std::uint64_t base_generation,
        const cbindgen_private::NativeSurfaceLayoutBatchData* source,
        void*)
    {
        if (!layout_callback_ || !source || !source->entries) return;
        for (std::size_t index = 0; index < source->entry_count; ++index) {
            const auto& entry = source->entries[index];
            if (entry.cluster_offset > source->cluster_count
                || entry.cluster_count > source->cluster_count - entry.cluster_offset) continue;
            const NativeSurfaceLayoutSnapshot snapshot {
                .layout_key = entry.layout_key,
                .baseline = entry.baseline,
                .advance = entry.advance,
                .clusters = reinterpret_cast<const NativeSurfaceLayoutCluster*>(source->clusters + entry.cluster_offset),
                .cluster_count = entry.cluster_count,
            };
            layout_callback_(surface_id, base_generation, snapshot, layout_user_data_);
        }
    }

    inline static layout_callback layout_callback_ = nullptr;
    inline static void* layout_user_data_ = nullptr;
};

} // namespace slint
