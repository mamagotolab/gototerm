#version 140

in vec2 position;
in vec2 tex_coords;
in uint is_bg;
in uint blinking;
in uvec2 color;

// ポップアップの出現アニメーション。pop_rect（NDC の left, bottom, right, top）に
// 入る頂点だけを pop_shift ずらし、フラグメント側で pop_alpha を掛ける。
uniform vec4 pop_rect;
uniform float pop_shift;

out vec2 v_tex_coords;
flat out uint v_is_bg;
flat out uvec2 v_color;
flat out uint v_blinking;
flat out uint v_in_pop;

void main() {
    const float EPS = 0.0001;
    bool in_pop = position.x >= pop_rect.x - EPS && position.x <= pop_rect.z + EPS
        && position.y >= pop_rect.y - EPS && position.y <= pop_rect.w + EPS;
    v_in_pop = in_pop ? 1u : 0u;
    gl_Position = vec4(position.x, position.y + (in_pop ? pop_shift : 0.0), 0, 1);
    v_tex_coords = tex_coords;
    v_is_bg = is_bg;
    v_color = color;
    v_blinking = blinking;
}
