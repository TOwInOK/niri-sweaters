precision highp float;

#if defined(DEBUG_FLAGS)
uniform float niri_tint;
#endif

uniform float niri_alpha;
uniform float niri_scale;

uniform vec2 niri_size;
varying vec2 niri_v_coords;

uniform float colorspace;
uniform float hue_interpolation;
uniform vec4 color_from;
uniform vec4 color_to;
uniform vec2 grad_offset;
uniform float grad_width;
uniform vec2 grad_vec;

uniform mat3 input_to_geo;
uniform vec2 geo_size;
uniform vec4 outer_radius;
uniform float border_width;

uniform float knit_enabled;
uniform float knit_pattern;
uniform vec4 knit_accent_color;
uniform float knit_stitch_size;
uniform float knit_relief;
uniform float knit_fuzz;
vec4 premul_rect(vec4 color) {
    color.rgb *= color.a;
    return color;
}

vec4 premul_lch(vec4 color) {
    color.xy *= color.a;
    return color;
}

vec4 unpremul_rect(vec4 color) {
    if (color.a == 0.0)
        return color;

    color.rgb /= color.a;
    return color;
}

vec4 unpremul_lch(vec4 color) {
    if (color.a == 0.0)
        return color;

    color.xy /= color.a;
    return color;
}

vec4 premul_mix_unpremul_rect(vec4 color1, vec4 color2, float ratio) {
    vec4 mixed = mix(premul_rect(color1), premul_rect(color2), ratio);
    return unpremul_rect(mixed);
}

vec4 premul_mix_unpremul_lch(vec4 color1, vec4 color2, float ratio) {
    vec4 mixed = mix(premul_lch(color1), premul_lch(color2), ratio);
    return unpremul_lch(mixed);
}

vec3 srgb_to_linear(vec3 color) {
    return pow(color, vec3(2.2));
}

vec3 linear_to_srgb(vec3 color) {
    return pow(color, vec3(1.0 / 2.2));
}

vec3 lab_to_lch(vec3 color) {
    float c = sqrt(pow(color.y, 2.0) + pow(color.z, 2.0));
    float h = degrees(atan(color.z, color.y)) ;
    h += h <= 0.0 ?
        360.0 :
        0.0 ;
    return vec3(
        color.x,
        c,
        h
    );
}

vec3 lch_to_lab(vec3 color) {
    float a = color.y * clamp(cos(radians(color.z)), -1.0, 1.0);
    float b = color.y * clamp(sin(radians(color.z)), -1.0, 1.0);
    return vec3(
        color.x,
        a,
        b
    );
}

vec3 linear_to_oklab(vec3 color){
    mat3 rgb_to_lms = mat3(
        vec3(0.4122214708, 0.5363325363, 0.0514459929),
        vec3(0.2119034982, 0.6806995451, 0.1073969566),
        vec3(0.0883024619, 0.2817188376, 0.6299787005)
    );
    mat3 lms_to_oklab = mat3(
        vec3(0.2104542553, 0.7936177850, -0.0040720468),
        vec3(1.9779984951, -2.4285922050, 0.4505937099),
        vec3(0.0259040371, 0.7827717662, -0.8086757660)
    );
    vec3 lms = color * rgb_to_lms;
    lms = pow(lms, vec3(1.0 / 3.0));
    return lms * lms_to_oklab;
}

vec3 oklab_to_linear(vec3 color){
    mat3 oklab_to_lms = mat3(
        vec3(1.0, 0.3963377774, 0.2158037573),
        vec3(1.0, -0.1055613458, -0.0638541728),
        vec3(1.0, -0.0894841775, -1.2914855480)
    );
    mat3 lms_to_rgb = mat3(
        vec3(4.0767416621, -3.3077115913, 0.2309699292),
        vec3(-1.2684380046, 2.6097574011, -0.3413193965),
        vec3(-0.0041960863, -0.7034186147, 1.7076147010)
    );
    vec3 lms = color * oklab_to_lms;
    lms = pow(lms, vec3(3.0));
    return lms * lms_to_rgb;
}

vec4 color_mix(vec4 color1, vec4 color2, float color_ratio) {
    vec4 color_out;

    // srgb
    if (colorspace == 0.0) {
        return mix(premul_rect(color1), premul_rect(color2), color_ratio);
    }

    color1.rgb = srgb_to_linear(color1.rgb);
    color2.rgb = srgb_to_linear(color2.rgb);

    // srgb-linear
    if (colorspace == 1.0) {
        color_out = premul_mix_unpremul_rect(color1, color2, color_ratio);
    // oklab
    } else if (colorspace == 2.0) {
        color1.xyz = linear_to_oklab(color1.rgb);
        color2.xyz = linear_to_oklab(color2.rgb);
        color_out = premul_mix_unpremul_rect(color1, color2, color_ratio);
        color_out.rgb = oklab_to_linear(color_out.xyz);
    // oklch
    } else if (colorspace == 3.0) {
        color1.xyz = lab_to_lch(linear_to_oklab(color1.rgb));
        color2.xyz = lab_to_lch(linear_to_oklab(color2.rgb));
        color_out = premul_mix_unpremul_lch(color1, color2, color_ratio);

        float min_hue = min(color1.z, color2.z);
        float max_hue = max(color1.z, color2.z);
        float path_direct_distance = (max_hue - min_hue) * color_ratio;
        float path_mod_distance = (360.0 - max_hue + min_hue) * color_ratio;

        float path_mod =
            color1.z == min_hue ?
                mod(color1.z - path_mod_distance, 360.0) :
                mod(color1.z + path_mod_distance, 360.0) ;
        float path_direct =
            color1.z == min_hue ?
                color1.z + path_direct_distance :
                color1.z - path_direct_distance ;

        // shorter
        if (hue_interpolation == 0.0) {
            color_out.z =
                max_hue - min_hue > 360.0 - max_hue + min_hue ?
                    path_mod :
                    path_direct ;
        // longer
        } else if (hue_interpolation == 1.0) {
            color_out.z =
                max_hue - min_hue <= 360.0 - max_hue + min_hue ?
                    path_mod :
                    path_direct ;
        // increasing
        } else if (hue_interpolation == 2.0) {
            color_out.z =
                color1.z > color2.z ?
                    path_mod :
                    path_direct ;
        // decreasing
        } else if (hue_interpolation == 3.0) {
            color_out.z =
                color1.z <= color2.z ?
                    path_mod :
                    path_direct ;
        }
        color_out.rgb = clamp(oklab_to_linear(lch_to_lab(color_out.xyz)), 0.0, 1.0);
    }

    return premul_rect(vec4(linear_to_srgb(color_out.rgb), color_out.a));
}

vec4 gradient_color(vec2 coords) {
    coords = coords + grad_offset;

    if ((grad_vec.x < 0.0 && 0.0 <= grad_vec.y) || (0.0 <= grad_vec.x && grad_vec.y < 0.0))
        coords.x -= grad_width;

    float frac = dot(coords, grad_vec) / dot(grad_vec, grad_vec);

    if (grad_vec.y < 0.0)
        frac += 1.0;

    frac = clamp(frac, 0.0, 1.0);
    return color_mix(color_from, color_to, frac);
}

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

float knit_hash(vec2 value) {
    return fract(sin(dot(value, vec2(127.1, 311.7))) * 43758.5453123);
}

float knit_capsule_distance(
    vec2 point,
    vec2 start,
    vec2 end,
    out vec2 delta,
    out float progress
) {
    vec2 from_start = point - start;
    vec2 segment = end - start;
    progress = clamp(dot(from_start, segment) / dot(segment, segment), 0.0, 1.0);
    delta = from_start - segment * progress;
    return length(delta);
}

vec4 knit_border_coordinates(vec2 point) {
    float width = max(border_width, 0.001);
    vec4 edge_distance = vec4(
        point.y,
        geo_size.x - point.x,
        geo_size.y - point.y,
        point.x
    );
    float nearest = min(min(edge_distance.x, edge_distance.y), min(edge_distance.z, edge_distance.w));
    vec4 corner_extent = max(outer_radius, vec4(width));
    float miter_distance = 1000000.0;
    if (edge_distance.w < corner_extent.x && edge_distance.x < corner_extent.x)
        miter_distance = abs(edge_distance.w - edge_distance.x) * 0.70710678;
    else if (edge_distance.x < corner_extent.y && edge_distance.y < corner_extent.y)
        miter_distance = abs(edge_distance.x - edge_distance.y) * 0.70710678;
    else if (edge_distance.y < corner_extent.z && edge_distance.z < corner_extent.z)
        miter_distance = abs(edge_distance.y - edge_distance.z) * 0.70710678;
    else if (edge_distance.z < corner_extent.w && edge_distance.w < corner_extent.w)
        miter_distance = abs(edge_distance.z - edge_distance.w) * 0.70710678;


    float along;
    float inward = nearest;
    float side_length;

    if (nearest == edge_distance.x) {
        // Top, from left to right.
        along = point.x;
        side_length = geo_size.x;
    } else if (nearest == edge_distance.y) {
        // Right, from top to bottom.
        along = point.y;
        side_length = geo_size.y;
    } else if (nearest == edge_distance.z) {
        // Bottom, from right to left.
        along = geo_size.x - point.x;
        side_length = geo_size.x;
    } else {
        // Left, from bottom to top.
        along = geo_size.y - point.y;
        side_length = geo_size.y;
    }

    side_length = max(side_length, 0.001);
    return vec4(
        clamp(along, 0.0, side_length),
        clamp(inward, 0.0, width),
        side_length,
        miter_distance
    );
}

float knit_accent_mix(float column, float row) {
    if (knit_pattern == 1.0)
        return mod(column, 3.0) < 1.0 ? 1.0 : 0.0;

    if (knit_pattern == 2.0)
        return mod(floor(column * 0.5) + floor(row * 0.5), 2.0);

    if (knit_pattern == 3.0) {
        float phase = mod(column + row * 2.0, 8.0);
        return phase < 1.5 || phase > 6.5 ? 1.0 : 0.0;
    }

    if (knit_pattern == 4.0) {
        float x = abs(mod(column, 8.0) - 4.0);
        float y = abs(mod(row, 6.0) - 3.0);
        return abs(x + y - 3.5) < 0.8 ? 1.0 : 0.0;
    }

    if (knit_pattern == 5.0)
        return mod(column, 6.0) < 1.0 && mod(row, 4.0) < 1.0 ? 1.0 : 0.0;

    return 0.0;
}

vec4 knit_color(vec2 geometry_coords, vec4 base_color) {
    vec4 border_coords = knit_border_coordinates(geometry_coords);
    float requested_stitch_width = max(knit_stitch_size, 1.0);
    float pattern_period = 1.0;
    if (knit_pattern == 1.0)
        pattern_period = 3.0;
    else if (knit_pattern == 2.0)
        pattern_period = 4.0;
    else if (knit_pattern == 3.0 || knit_pattern == 4.0)
        pattern_period = 8.0;
    else if (knit_pattern == 5.0)
        pattern_period = 6.0;

    float repeats = max(1.0, floor(border_coords.z / (requested_stitch_width * pattern_period) + 0.5));
    float stitch_width = border_coords.z / (repeats * pattern_period);
    // Tighter rows and plump curved strands fill the band like chunky knitwear.
    float stitch_height = stitch_width * 0.68;
    float row = floor(border_coords.y / stitch_height);
    float along = border_coords.x / stitch_width + mod(row, 2.0) * 0.5;
    float column = floor(along);
    float stitch_seed = knit_hash(vec2(column * 17.0 + row * 0.37, row * 29.0));
    vec2 cell = vec2(fract(along) - 0.5, fract(border_coords.y / stitch_height) - 0.5);
    cell += vec2(stitch_seed - 0.5, knit_hash(vec2(column, row + 41.0)) - 0.5)
        * vec2(0.035, 0.018);

    vec2 left_top_delta;
    vec2 left_bottom_delta;
    vec2 right_top_delta;
    vec2 right_bottom_delta;
    float left_top_progress;
    float left_bottom_progress;
    float right_top_progress;
    float right_bottom_progress;

    vec2 left_top_start = vec2(-0.42, -0.50);
    vec2 left_middle = vec2(-0.18, 0.0);
    vec2 stitch_apex = vec2(0.0, 0.47);
    vec2 right_middle = vec2(0.18, 0.0);
    vec2 right_top_start = vec2(0.42, -0.50);

    float left_top_distance = knit_capsule_distance(
        cell, left_top_start, left_middle, left_top_delta, left_top_progress
    );
    float left_bottom_distance = knit_capsule_distance(
        cell, left_middle, stitch_apex, left_bottom_delta, left_bottom_progress
    );
    float right_top_distance = knit_capsule_distance(
        cell, right_top_start, right_middle, right_top_delta, right_top_progress
    );
    float right_bottom_distance = knit_capsule_distance(
        cell, right_middle, stitch_apex, right_bottom_delta, right_bottom_progress
    );

    float left_distance;
    float left_progress;
    vec2 left_delta;
    vec2 left_tangent;
    if (left_top_distance < left_bottom_distance) {
        left_distance = left_top_distance;
        left_progress = left_top_progress * 0.5;
        left_delta = left_top_delta;
        left_tangent = left_middle - left_top_start;
    } else {
        left_distance = left_bottom_distance;
        left_progress = 0.5 + left_bottom_progress * 0.5;
        left_delta = left_bottom_delta;
        left_tangent = stitch_apex - left_middle;
    }

    float right_distance;
    float right_progress;
    vec2 right_delta;
    vec2 right_tangent;
    if (right_top_distance < right_bottom_distance) {
        right_distance = right_top_distance;
        right_progress = right_top_progress * 0.5;
        right_delta = right_top_delta;
        right_tangent = right_middle - right_top_start;
    } else {
        right_distance = right_bottom_distance;
        right_progress = 0.5 + right_bottom_progress * 0.5;
        right_delta = right_bottom_delta;
        right_tangent = stitch_apex - right_middle;
    }

    bool use_left = left_distance < right_distance;
    float strand_distance = use_left ? left_distance : right_distance;
    float strand_progress = use_left ? left_progress : right_progress;
    vec2 strand_delta = use_left ? left_delta : right_delta;
    vec2 strand_tangent = normalize(use_left ? left_tangent : right_tangent);

    float relief = clamp(knit_relief, 0.0, 1.0);
    float fuzz = clamp(knit_fuzz, 0.0, 1.0);
    float fibre_noise = knit_hash(floor(geometry_coords * niri_scale * 2.25));
    float coarse_fuzz = knit_hash(floor(geometry_coords * niri_scale * 0.75) + stitch_seed * 31.0);
    float yarn_radius = mix(0.20, 0.27, relief)
        * mix(0.96, 1.04, stitch_seed)
        * mix(0.985, 1.015, fibre_noise)
        * mix(1.0, 1.04, fuzz);
    float edge_variation = (coarse_fuzz - 0.5) * yarn_radius * fuzz * 0.16;
    float effective_distance = max(0.0, strand_distance + edge_variation);
    float antialias = 1.0 / max(niri_scale * stitch_width, 1.0);

    float spine_half_width = yarn_radius * stitch_width;
    float spine_antialias = 1.0 / max(niri_scale, 1.0);
    float spine = 1.0 - smoothstep(
        spine_half_width - spine_antialias,
        spine_half_width + spine_antialias,
        border_coords.w
    );
    float accent_visibility = smoothstep(
        spine_half_width,
        spine_half_width + stitch_width * 1.25,
        border_coords.w
    );

    float strand = 1.0 - smoothstep(
        yarn_radius - antialias,
        yarn_radius + antialias,
        effective_distance
    );
    float normalized_distance = clamp(effective_distance / yarn_radius, 0.0, 1.0);
    float height = sqrt(max(1.0 - normalized_distance * normalized_distance, 0.0));
    vec3 normal = normalize(vec3(
        strand_delta / max(yarn_radius, 0.001) * relief,
        1.0
    ));

    // Matte diffuse yarn: broad light, almost no plastic specular.
    vec3 light = normalize(vec3(-0.45, -0.65, 0.9));
    vec3 half_vector = normalize(light + vec3(0.0, 0.0, 1.0));
    float diffuse = max(dot(normal, light), 0.0);
    float soft_sheen = pow(max(dot(normal, half_vector), 0.0), 4.0)
        * 0.025 * relief * (1.0 - fuzz * 0.85);
    float lighting = 0.78 + diffuse * 0.15 + height * relief * 0.07 + soft_sheen;

    // Three quiet twisted plies follow the selected strand tangent.
    vec2 strand_normal = vec2(-strand_tangent.y, strand_tangent.x);
    float cross_section = dot(strand_delta, strand_normal) / max(yarn_radius, 0.001);
    float ply_phase = strand_progress * 18.8495559
        + cross_section * 2.4
        + stitch_seed * 6.2831853;
    float ply_shade = 1.0 + sin(ply_phase) * 0.025 * relief;
    float stitch_tone = mix(0.96, 1.04, stitch_seed);
    float fibre_tone = mix(0.98 - fuzz * 0.05, 1.02 + fuzz * 0.05, fibre_noise);
    // Upper arms tuck under the previous course instead of forming a flat X lattice.
    float tuck_shadow = mix(0.76, 1.0, smoothstep(-0.50, 0.05, cell.y));

    vec4 accent = premul_rect(knit_accent_color);
    float accent_mix = knit_accent_mix(column, row) * accent_visibility;
    vec4 yarn = mix(base_color, accent, accent_mix);
    yarn.rgb = min(
        yarn.rgb * lighting * ply_shade * stitch_tone * fibre_tone * tuck_shadow,
        vec3(yarn.a)
    );

    // Dense base fabric and soft self-occlusion between overlapping loops.
    float shadow_width = mix(0.10, 0.18, relief);
    float contact_shadow = 1.0 - smoothstep(
        yarn_radius,
        yarn_radius + shadow_width,
        effective_distance
    );
    vec4 ground = base_color;
    float course = 0.60 + 0.06 * sin(border_coords.y * 3.0 / stitch_height);
    ground.rgb *= course * (1.0 - contact_shadow * relief * 0.30);
    vec4 fabric = mix(ground, yarn, strand);

    // Soft halo and sparse flyaways stay in stitch-local coordinates, so they
    // remain stable while windows move and resize.
    float halo_width = yarn_radius * mix(0.12, 0.75, fuzz);
    float halo_proximity = 1.0 - smoothstep(
        yarn_radius + antialias,
        yarn_radius + halo_width + antialias,
        effective_distance
    );
    float flyaway_noise = knit_hash(
        floor(geometry_coords * niri_scale * 1.35) + vec2(stitch_seed * 53.0, row)
    );
    float flyaways = smoothstep(0.88, 0.995, flyaway_noise);
    float fuzz_mask = clamp(
        halo_proximity * fuzz * (0.18 + flyaways * 0.42) * (1.0 - strand),
        0.0,
        0.55
    );
    vec4 fuzzy_yarn = yarn;
    fuzzy_yarn.rgb *= mix(0.86, 1.02, fibre_noise);
    fabric = mix(fabric, fuzzy_yarn, fuzz_mask);

    float spine_distance = clamp(border_coords.w / max(spine_half_width, 0.001), 0.0, 1.0);
    float spine_height = sqrt(max(1.0 - spine_distance * spine_distance, 0.0));
    vec4 spine_yarn = base_color;
    spine_yarn.rgb *= (0.76 + spine_height * relief * 0.20)
        * mix(0.98, 1.02, fibre_noise);
    return mix(fabric, spine_yarn, spine);
}
void main() {
    vec3 coords_geo = input_to_geo * vec3(niri_v_coords, 1.0);
    vec4 base_color = gradient_color(coords_geo.xy);
    vec4 color = knit_enabled == 1.0
        ? knit_color(coords_geo.xy, base_color)
        : base_color;
    float ring_alpha = niri_rounding_alpha(coords_geo.xy, geo_size, outer_radius);

    if (border_width > 0.0) {
        vec2 inner_coords = coords_geo.xy - vec2(border_width);
        vec2 inner_geo_size = geo_size - vec2(border_width * 2.0);
        if (0.0 <= inner_coords.x && inner_coords.x <= inner_geo_size.x
                && 0.0 <= inner_coords.y && inner_coords.y <= inner_geo_size.y)
        {
            vec4 inner_radius = max(outer_radius - vec4(border_width), 0.0);
            ring_alpha *= 1.0 - niri_rounding_alpha(inner_coords, inner_geo_size, inner_radius);
        }
    }

    color *= ring_alpha * niri_alpha;
#if defined(DEBUG_FLAGS)
    if (niri_tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
