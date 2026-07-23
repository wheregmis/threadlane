//! Cross-panel primitive widgets and reusable Makepad DSL components.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components = {}

    mod.components.PanelHeader = View {
        width: Fill
        height: Fit
        flow: Right
        align: Align{y: 0.5}
        padding: Inset{left: 8 right: 8 top: 2 bottom: 2}
    }

    mod.components.PanelSurface = RoundedView {
        width: Fill
        height: Fill
        draw_bg +: { color: #x1f232b border_radius: 10.0 }
    }

    mod.components.EmptyRowBase = View {
        width: Fill
        height: Fit
        lbl := Label {
            width: Fill
            height: Fit
            text: ""
            draw_text +: { color: #x6f7a88 text_style +: { font_size: 10.0 } }
        }
    }

    mod.components.ProgressDot = RoundedView {
        width: 3
        height: 3
        draw_bg +: { color: #x8b93a0 border_radius: 1.5 }
    }

    mod.components.ActivityLoader = View {
        width: 20
        height: 10
        show_bg: true
        draw_bg +: {
            color: uniform(#x70a7ff)
            color_mid: uniform(#x8c8df4)
            color_tail: uniform(#x9c72d8)
            speed: uniform(3.2)
            dot_radius: uniform(1.15)

            pixel: fn() {
                let p = self.pos * self.rect_size
                let center = self.rect_size * 0.5
                let orbit = vec2(self.rect_size.x * 0.29, self.rect_size.y * 0.24)
                let angle = self.draw_pass.time * self.speed

                let p0 = center + vec2(cos(angle), sin(angle)) * orbit
                let p1 = center + vec2(cos(angle - 0.72), sin(angle - 0.72)) * orbit
                let p2 = center + vec2(cos(angle - 1.44), sin(angle - 1.44)) * orbit

                let d0 = length(p - p0)
                let d1 = length(p - p1)
                let d2 = length(p - p2)

                let a0 = smoothstep(self.dot_radius + 0.8, self.dot_radius - 0.45, d0)
                let a1 = smoothstep(self.dot_radius + 0.65, self.dot_radius - 0.35, d1) * 0.72
                let a2 = smoothstep(self.dot_radius + 0.5, self.dot_radius - 0.25, d2) * 0.42

                let g0 = smoothstep(self.dot_radius + 2.6, self.dot_radius, d0) * 0.20
                let g1 = smoothstep(self.dot_radius + 2.2, self.dot_radius, d1) * 0.11
                let g2 = smoothstep(self.dot_radius + 1.8, self.dot_radius, d2) * 0.06

                let w0 = a0 + g0
                let w1 = a1 + g1
                let w2 = a2 + g2
                let energy = w0 + w1 + w2
                let rgb = (
                    self.color.xyz * w0
                    + self.color_mid.xyz * w1
                    + self.color_tail.xyz * w2
                ) / max(energy, 0.001)

                return Pal.premul(vec4(rgb, clamp(energy, 0.0, 1.0)))
            }
        }
    }

    mod.components.ComposerSurface = RoundedView {
        width: Fill
        height: Fit
        draw_bg +: {
            color: #x1f232b
            border_color: #x3a424e
            border_size: 1.0
            border_radius: 11.0
        }
    }

    mod.components.ComposerChip = Button {
        width: Fit
        height: 24
        padding: Inset{left: 9 right: 9 top: 2 bottom: 2}
        draw_bg +: {
            color: #x232830
            color_hover: #x2a313c
            color_down: #x354153
            border_color: #x3a424e
            border_color_hover: #x4a5564
            border_size: 1.0
            border_radius: 6.0
        }
        draw_text +: {
            color: #xc7cdd6
            color_hover: #xdde3ea
            color_down: #xffffff
            text_style +: { font_size: 9.0 }
        }
    }

    mod.components.ComposerAction = Button {
        width: Fit
        height: 28
        padding: Inset{left: 11 right: 11 top: 2 bottom: 2}
        draw_bg +: {
            color: #x4f78aa
            color_hover: #x6092cc
            color_down: #x70a7ff
            border_radius: 7.0
        }
        draw_text +: {
            color: #xffffff
            text_style: theme.font_bold { font_size: 9.5 }
        }
    }

    mod.components.FlexSpacer = View { width: Fill height: 1 }
}
