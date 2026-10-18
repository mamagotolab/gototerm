#version 140

uniform sampler2D tex;
uniform float timestamp;
// 見た目の効果（view.rs の Effects）。frame_rect はペイン枠の (left, bottom, w, h)
// をフレームバッファ座標で、screen はフレームバッファ全体の大きさ。
uniform vec4 frame_rect;
uniform vec2 screen;
uniform float radius;
uniform float dim;
uniform float flash;
uniform int effect;
uniform float pop_alpha;
flat in uint v_in_pop;
in vec2 v_tex_coords;
flat in uint v_is_bg;
flat in uint v_blinking;
flat in uvec2 v_color;

vec3 hsv2rgb(float h, float s, float v) {
    return ((clamp(abs(fract(h + vec3(0.0, 2.0, 1.0) / 3.0) * 6.0 - 3.0) - 1.0, 0.0, 1.0) - 1.0) * s + 1.0) * v;
}

vec4 get_color(uint color) {
    if (color == 0xFFFFFF00u) {
        return vec4(hsv2rgb(timestamp / 3000.0, 1.0, 1.0), 1.0);
    } else {
        uint r = (color & 0xFF000000u) >> 24;
        uint g = (color & 0x00FF0000u) >> 16;
        uint b = (color & 0x0000FF00u) >> 8;
        uint a = (color & 0x000000FFu);
        return vec4(float(r) / 255.0, float(g) / 255.0, float(b) / 255.0, float(a) / 255.0);
    }
}

void main() {
    vec4 back = get_color(v_color[0]);
    vec4 fore = get_color(v_color[1]);

    uint phase = uint(timestamp / 250.0);
    if (v_blinking == 1u && phase % 8u < 4u) {
        fore = back;
    } else if (v_blinking == 2u && phase % 2u < 1u) {
        fore = back;
    }

    vec4 c;
    if (v_is_bg == 1u) {
        c = back;
    } else {
        float a = texture(tex, v_tex_coords).r;
        c = vec4(fore.rgb, a);
    }

    // 角丸：枠の外は捨て、縁の1pxだけアルファで滑らかにする。
    if (radius > 0.0) {
        vec2 half_size = frame_rect.zw * 0.5;
        vec2 q = abs(gl_FragCoord.xy - frame_rect.xy - half_size) - (half_size - vec2(radius));
        float d = length(max(q, 0.0)) - radius;
        if (d > 0.5) {
            discard;
        }
        c.a *= clamp(0.5 - d, 0.0, 1.0);
    }

    c.rgb *= 1.0 - dim;
    c.rgb = mix(c.rgb, vec3(1.0), flash);

    // CRT 風：走査線と周辺減光、わずかな蛍光体の緑み。走査線は背景だけに
    // かける（文字に載せると横縞で欠けて解像度が落ちたように見える）。
    if (effect == 1) {
        if (v_is_bg == 1u && mod(gl_FragCoord.y, 3.0) < 1.0) {
            c.rgb *= 0.85;
        }
        vec2 uv = gl_FragCoord.xy / max(screen, vec2(1.0)) - 0.5;
        c.rgb *= clamp(1.0 - dot(uv, uv) * 0.9, 0.0, 1.0);
        c.rgb *= vec3(0.96, 1.04, 0.98);
    }

    if (v_in_pop == 1u) {
        c.a *= pop_alpha;
    }

    gl_FragColor = c;
}
