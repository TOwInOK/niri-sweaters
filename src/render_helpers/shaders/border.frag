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
uniform float grad_inv_dot;

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

vec3 linear_to_srgb(vec3 color) {
    return pow(color, vec3(1.0 / 2.2));
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
    // Multiplication instead of pow(): lms can go negative out of gamut,
    // where pow() is undefined.
    lms = lms * lms * lms;
    return lms * lms_to_rgb;
}

// color_from/color_to arrive already converted into the interpolation space
// (srgb, linear, oklab or oklch); the conversion runs once per element on the
// CPU instead of per pixel.
vec4 color_mix(vec4 color1, vec4 color2, float color_ratio) {
    vec4 color_out;

    // srgb
    if (colorspace == 0.0) {
        return mix(premul_rect(color1), premul_rect(color2), color_ratio);
    }

    // srgb-linear
    if (colorspace == 1.0) {
        color_out = premul_mix_unpremul_rect(color1, color2, color_ratio);
    // oklab
    } else if (colorspace == 2.0) {
        color_out = premul_mix_unpremul_rect(color1, color2, color_ratio);
        color_out.rgb = oklab_to_linear(color_out.xyz);
    // oklch
    } else if (colorspace == 3.0) {
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

    float frac = dot(coords, grad_vec) * grad_inv_dot;

    if (grad_vec.y < 0.0)
        frac += 1.0;

    frac = clamp(frac, 0.0, 1.0);
    return color_mix(color_from, color_to, frac);
}

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

float knit_hash(vec2 value) {
    return fract(sin(dot(value, vec2(127.1, 311.7))) * 43758.5453123);
}

// Value and analytic derivatives of a smooth fibre field. Anisotropic sampling
// makes irregular bundles, rather than equally spaced grooves in a smooth cord.
vec3 knit_noise(vec2 p) {
    vec2 cell = floor(p);
    vec2 f = fract(p);
    vec2 blend = f * f * (3.0 - 2.0 * f);
    vec2 derivative = 6.0 * f * (1.0 - f);
    float a = knit_hash(cell);
    float b = knit_hash(cell + vec2(1.0, 0.0));
    float c = knit_hash(cell + vec2(0.0, 1.0));
    float d = knit_hash(cell + vec2(1.0, 1.0));
    return vec3(
        mix(mix(a, b, blend.x), mix(c, d, blend.x), blend.y),
        mix(b - a, d - c, blend.y) * derivative.x,
        mix(c - a, d - b, blend.x) * derivative.y
    );
}

// Box-filtered, finite wool fibres. Random length, slant and spacing keep the
// pile from turning into the continuous, parallel highlights of synthetic cord.
vec2 knit_filaments(vec2 p, vec2 footprint) {
    vec2 cell = floor(p);
    vec2 q = fract(p) - 0.5;
    float seed = knit_hash(cell);
    q.x -= (seed - 0.5) * 0.46 + q.y * (fract(seed * 13.7) - 0.5) * 0.55;
    q.x += sin(q.y * 5.0 + seed * 17.0) * 0.055;
    float half_length = mix(0.20, 0.46, fract(seed * 7.3));
    float radius = mix(0.055, 0.11, fract(seed * 23.1));
    vec2 aa = max(footprint, vec2(0.001));
    float ends = 1.0 - smoothstep(half_length - aa.y * 0.5, half_length + aa.y * 0.5, abs(q.y));
    float fibre = clamp((q.x + radius) / aa.x + 0.5, 0.0, 1.0)
        - clamp((q.x - radius) / aa.x + 0.5, 0.0, 1.0);
    float shadow = clamp((q.x + radius - 0.14) / aa.x + 0.5, 0.0, 1.0)
        - clamp((q.x - radius - 0.14) / aa.x + 0.5, 0.0, 1.0);
    return vec2(fibre, shadow) * ends * (1.0 - smoothstep(0.8, 2.0, aa.x));
}

// Elliptical yarn bodies bend towards the loop tip. Unlike constant-width
// capsules, their ends disappear into the adjoining course instead of making Xs.
vec4 knit_leg(vec2 point, float side, float seed, float radius, out vec3 normal, out vec2 tangent) {
    vec2 start = vec2(side * 0.40, -0.56);
    vec2 axis = vec2(-side * 0.34, 1.12);
    float axis_squared = dot(axis, axis);
    float t = clamp(dot(point - start, axis) / axis_squared, 0.0, 1.0);
    float bow = side * (0.035 + 0.012 * seed);
    vec2 center = start + axis * t + vec2(bow * 4.0 * t * (1.0 - t), 0.0);
    tangent = axis + vec2(bow * (4.0 - 8.0 * t), 0.0);
    t = clamp(t + dot(point - center, tangent) / dot(tangent, tangent), 0.0, 1.0);
    center = start + axis * t + vec2(bow * 4.0 * t * (1.0 - t), 0.0);
    tangent = normalize(axis + vec2(bow * (4.0 - 8.0 * t), 0.0));
    vec2 across = vec2(tangent.y, -tangent.x);
    float u = dot(point - center, across) / radius;
    float v = t * 2.0 - 1.0;
    // Include distance past the endpoints; clamping t must not extrude the tips.
    float end_distance = dot(point - center, tangent);
    // Keep a narrow yarn neck at each end: the strand tucks into the next
    // course instead of ending as a separate pointed bead.
    float ellipse = u * u + v * v * 0.86 + end_distance * end_distance / (radius * radius);
    float dome = sqrt(max(1.0 - ellipse, 0.0));
    normal = vec3(across * u + tangent * (v * radius * 1.46), max(dome / 0.78, 0.08));
    return vec4((sqrt(ellipse) - 1.0) * radius, dome * radius, u, t);
}

// Each course is its own inset rounded rectangle. Counts follow that course's
// arc length; individual loops are evaluated in a rigid tangent frame, never
// stretched by a polar UV or cross-faded with the neighbouring side.
struct KnitCourse {
    vec2 size;
    vec4 radius;
    vec4 edges;
    vec4 bends;
    float depth;
};

KnitCourse knit_course(float depth, float stitch_width) {
    KnitCourse course;
    course.depth = depth;
    course.size = max(geo_size - vec2(2.0 * depth), vec2(0.001));
    // Clockwise, beginning with the corner at the end of the top edge.
    course.radius = max(outer_radius.yzwx - vec4(depth), vec4(0.0));
    vec4 lengths = max(vec4(course.size.x, course.size.y, course.size.x, course.size.y)
        - course.radius.wxyz - course.radius, vec4(0.0));
    course.edges = max(floor(lengths / stitch_width + 0.5), vec4(1.0)) * step(0.001, lengths);
    course.bends = max(floor(course.radius * 1.57079633 / stitch_width + 0.5), vec4(1.0))
        * step(0.001, course.radius);
    return course;
}

float knit_course_coordinate(vec2 point, KnitCourse course) {
    vec2 p = point - vec2(course.depth);
    vec2 size = course.size;
    vec4 r = course.radius;
    vec4 e = course.edges;
    vec4 b = course.bends;
    float right = e.x + b.x;
    float bottom = right + e.y + b.y;
    float left = bottom + e.z + b.z;
    if (p.x < r.w && p.y < r.w)
        return left + e.w + atan(r.w - p.y, r.w - p.x) * 0.63661977 * b.w;
    if (p.x > size.x - r.x && p.y < r.x)
        return e.x + atan(p.x - size.x + r.x, r.x - p.y) * 0.63661977 * b.x;
    if (p.x > size.x - r.y && p.y > size.y - r.y)
        return right + e.y + atan(p.y - size.y + r.y, p.x - size.x + r.y) * 0.63661977 * b.y;
    if (p.x < r.z && p.y > size.y - r.z)
        return bottom + e.z + atan(r.z - p.x, p.y - size.y + r.z) * 0.63661977 * b.z;
    vec4 distance = vec4(p.y, size.x - p.x, size.y - p.y, p.x);
    float nearest = min(min(distance.x, distance.y), min(distance.z, distance.w));
    if (nearest == distance.x)
        return clamp((p.x - r.w) / max(size.x - r.w - r.x, 0.001), 0.0, 1.0) * e.x;
    if (nearest == distance.y)
        return right + clamp((p.y - r.x) / max(size.y - r.x - r.y, 0.001), 0.0, 1.0) * e.y;
    if (nearest == distance.z)
        return bottom + clamp((size.x - r.y - p.x) / max(size.x - r.y - r.z, 0.001), 0.0, 1.0) * e.z;
    return left + clamp((size.y - r.z - p.y) / max(size.y - r.z - r.w, 0.001), 0.0, 1.0) * e.w;
}

void knit_course_stitch(
    float coordinate, KnitCourse course, KnitCourse motif,
    out vec2 center, out vec2 tangent, out float width, out float column
) {
    float s = mod(coordinate, max(dot(course.edges + course.bends, vec4(1.0)), 1.0));
    float motif_start = 0.0;
    center = vec2(0.0);
    tangent = vec2(1.0, 0.0);
    width = max(knit_stitch_size, 1.0);
    column = 0.0;
    for (int side = 0; side < 4; side++) {
        vec2 start;
        if (side == 0) {
            start = vec2(course.radius.w, 0.0);
            tangent = vec2(1.0, 0.0);
        } else if (side == 1) {
            start = vec2(course.size.x, course.radius.x);
            tangent = vec2(0.0, 1.0);
        } else if (side == 2) {
            start = vec2(course.size.x - course.radius.y, course.size.y);
            tangent = vec2(-1.0, 0.0);
        } else {
            start = vec2(0.0, course.size.y - course.radius.z);
            tangent = vec2(0.0, -1.0);
        }
        int previous = side == 0 ? 3 : side - 1;
        float length = (side == 0 || side == 2 ? course.size.x : course.size.y)
            - course.radius[previous] - course.radius[side];
        float edges = course.edges[side];
        float bends = course.bends[side];
        if (s < edges) {
            float fraction = s / edges;
            center = start + tangent * (fraction * length) + vec2(course.depth);
            width = length / edges;
            column = floor(motif_start + fraction * motif.edges[side]);
            return;
        }
        s -= edges;
        motif_start += motif.edges[side];
        if (s < bends) {
            float fraction = s / bends;
            float angle = fraction * 1.57079633;
            vec2 inward = vec2(-tangent.y, tangent.x);
            float radius = course.radius[side];
            center = start + tangent * length + inward * radius
                + radius * (tangent * sin(angle) - inward * cos(angle)) + vec2(course.depth);
            tangent = tangent * cos(angle) + inward * sin(angle);
            width = 2.0 * radius * sin(0.78539816 / bends);
            column = floor(motif_start + fraction * motif.bends[side]);
            return;
        }
        s -= bends;
        motif_start += motif.bends[side];
    }
}

// Inward normal and signed depth from the true outline, including asymmetric radii.
vec3 knit_border_frame(vec2 point) {
    vec4 distance = vec4(point.y, geo_size.x - point.x, geo_size.y - point.y, point.x);
    float depth = min(min(distance.x, distance.y), min(distance.z, distance.w));
    // At a sharp inner corner the distance gradient has two valid directions.
    // Average only that subpixel normal crease, not the yarn or motif.
    vec4 weights = max(vec4(1.0) - (distance - vec4(depth))
        / max(0.75 / niri_scale, 0.001), vec4(0.0));
    vec2 inward = vec2(weights.w - weights.y, weights.x - weights.z);
    inward /= max(length(inward), 0.001);

    vec2 center;
    float radius;
    if (point.x < outer_radius.x && point.y < outer_radius.x) {
        radius = outer_radius.x;
        center = vec2(radius);
    } else if (point.x > geo_size.x - outer_radius.y && point.y < outer_radius.y) {
        radius = outer_radius.y;
        center = vec2(geo_size.x - radius, radius);
    } else if (point.x > geo_size.x - outer_radius.z && point.y > geo_size.y - outer_radius.z) {
        radius = outer_radius.z;
        center = geo_size - vec2(radius);
    } else if (point.x < outer_radius.w && point.y > geo_size.y - outer_radius.w) {
        radius = outer_radius.w;
        center = vec2(radius, geo_size.y - radius);
    } else {
        return vec3(inward, depth);
    }
    vec2 delta = center - point;
    float radial_distance = length(delta);
    return vec3(delta / max(radial_distance, 0.001), radius - radial_distance);
}

float knit_accent_mix(float column, float row) {
    if (knit_pattern == 1.0)
        return mod(column, 3.0) < 1.0 ? 1.0 : 0.0;
    if (knit_pattern == 2.0)
        return mod(floor(column * 0.5) + floor(row * 0.5), 2.0);
    if (knit_pattern == 3.0) {
        float peak = abs(mod(column, 8.0) - 4.0);
        return abs(mod(row, 6.0) - peak) < 0.5 ? 1.0 : 0.0;
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

vec4 knit_fabric(vec2 geometry_coords, float stitch_width, vec4 base_color, KnitCourse motif) {
    float relief = clamp(knit_relief, 0.0, 1.0);
    float fuzz = clamp(knit_fuzz, 0.0, 1.0);
    float stitch_height = stitch_width * 0.94;
    vec3 frame = knit_border_frame(geometry_coords);
    float roll = clamp(2.0 * frame.z / max(border_width, 0.001) - 1.0, -1.0, 1.0);
    float slope = roll / sqrt(max(1.0 - roll * roll, 0.20));
    vec3 band_normal = normalize(vec3(frame.xy * slope * 0.38 * relief, 1.0));
    float grid_row = frame.z / stitch_height;
    float base_row = floor(grid_row);
    vec4 strand = vec4(100.0, 0.0, 0.0, 0.0);
    vec3 strand_normal = vec3(0.0, 0.0, 1.0);
    float best_surface = -100.0;
    float yarn_seed = 0.0;
    float yarn_row = base_row;
    float yarn_column = 0.0;
    float yarn_width = stitch_width;
    vec2 yarn_tangent = vec2(0.0, 1.0);

    for (int neighbour = 0; neighbour < 2; neighbour++) {
        float row = base_row + (neighbour == 0 ? 0.0 : (fract(grid_row) < 0.5 ? -1.0 : 1.0));
        float depth = (row + 0.5) * stitch_height;
        if (2.0 * depth >= min(geo_size.x, geo_size.y))
            continue;
        KnitCourse course = knit_course(depth, stitch_width);
        float along = knit_course_coordinate(geometry_coords, course);
        float cell = floor(along);
        for (int adjacent = -1; adjacent <= 1; adjacent++) {
            float index = cell + float(adjacent);
            vec2 center;
            vec2 tangent;
            float width;
            float column;
            knit_course_stitch(index + 0.5, course, motif, center, tangent, width, column);
            // Tight inner turns decrease the stitch count, not the yarn diameter.
            width = clamp(width, stitch_width * 0.82, stitch_width * 1.18);
            vec2 inward = vec2(-tangent.y, tangent.x);
            vec2 delta = geometry_coords - center;
            vec2 point = vec2(dot(delta, tangent) / width, dot(delta, inward) / stitch_width);
            float stitch_id = mod(index, max(dot(course.edges + course.bends, vec4(1.0)), 1.0));
            float seed = knit_hash(vec2(stitch_id, row));
            point += vec2(seed - 0.5, fract(seed * 13.73) - 0.5) * vec2(0.018, 0.024);
            float radius = mix(0.225, 0.268, relief) * mix(0.96, 1.04, seed);
            // Legs reach at most ~0.75/0.85 stitch units. Once some yarn
            // covers this pixel (best_surface >= 0), far cells evaluate to a
            // negative surface and cannot win, so skip the capsule math.
            if (best_surface >= 0.0 && (abs(point.x) > 0.8 || abs(point.y) > 0.9))
                continue;
            for (int leg = 0; leg < 2; leg++) {
                vec3 normal;
                vec2 leg_tangent;
                // The two yarn legs have independent thickness, not coincident
                // mirror surfaces with an undefined frontmost normal at the cleft.
                float leg_radius = radius * mix(0.98, 1.02, fract(seed * 7.17 + float(leg) * 0.5));
                vec4 candidate = knit_leg(point, leg == 0 ? -1.0 : 1.0, seed, leg_radius, normal, leg_tangent);
                float surface = candidate.y - max(candidate.x, 0.0);
                if (surface > best_surface) {
                    best_surface = surface;
                    strand = candidate;
                    strand_normal = vec3(tangent * normal.x * (stitch_width / width)
                        + inward * normal.y, normal.z);
                    yarn_seed = seed;
                    yarn_row = row;
                    yarn_column = column;
                    yarn_width = min(width, stitch_width);
                    yarn_tangent = normalize(tangent * leg_tangent.x * width
                        + inward * leg_tangent.y * stitch_width);
                }
            }
        }
    }
    float pixel = 1.0 / max(niri_scale * yarn_width, 1.0);

    float u = strand.z;
    float t = strand.w;
    float bundle_filter = 1.0 - smoothstep(0.6, 1.6, pixel * 7.2);
    float fibre_filter = 1.0 - smoothstep(0.8, 2.0, pixel * 32.0);
    float nap_filter = 1.0 - smoothstep(0.8, 2.0, pixel * 52.0);
    vec3 bundles = knit_noise(vec2(u * 1.8 + t * 2.5, t * 0.65) + yarn_seed * vec2(19.0, 31.0));
    vec3 fibres = fibre_filter > 0.0
        ? knit_noise(vec2(u * 8.0 + t * 2.8 + bundles.x * 0.8, t * 3.5)
            + yarn_seed * vec2(43.0, 17.0))
        : vec3(0.0);
    vec3 nap = nap_filter > 0.0
        ? knit_noise(vec2(u * 13.0 - t * 4.0, t * 9.0) + yarn_seed * vec2(11.0, 53.0))
        : vec3(0.0);
    float pile = 0.18 + fuzz * 0.82;
    // knit_filaments fades out by aa.x = 2.0; skip the call entirely past it.
    vec2 filaments = 13.6 * pixel < 2.0
        ? knit_filaments(vec2(u * 3.4 + t * 1.6 + bundles.x * 0.3, t * 5.0)
            + yarn_seed * vec2(29.0, 47.0), vec2(13.6, 4.5) * pixel)
        : vec2(0.0);
    float thickness = (bundles.x - 0.5) * 0.035 * bundle_filter;
    float loose_fibres = (nap.x - 0.5) * 0.045 * pile * nap_filter;
    float distance = strand.x - thickness - loose_fibres;
    float coverage = 1.0 - smoothstep(-pixel * 0.65, pixel * 0.65, distance);

    // The roughness changes the surface normal as well as the colour. Short
    // fibres scatter light across the shoulders instead of making a shiny rim.
    vec2 across = vec2(yarn_tangent.y, -yarn_tangent.x);
    vec2 roughness = across * (bundles.y * 0.42 * bundle_filter + fibres.y * 0.12 * fibre_filter)
        + yarn_tangent * (bundles.z * 0.055 * bundle_filter + nap.z * 0.07 * pile * nap_filter);
    vec3 normal = normalize(vec3(
        (strand_normal.xy + roughness) * relief + band_normal.xy * strand_normal.z,
        strand_normal.z * band_normal.z
    ));
    vec3 light = normalize(vec3(-0.45, -0.65, 1.1));
    float diffuse = max((dot(normal, light) + 0.20) / 1.20, 0.0);
    float shoulder = (1.0 - normal.z) * (1.0 - normal.z);
    float lighting = 0.48 + diffuse * 0.52 + shoulder * (0.10 + pile * 0.05);
    float tuck = mix(0.74, 1.0, smoothstep(0.02, 0.34, t));
    float edge_occlusion = mix(0.86, 1.0, smoothstep(0.0, 0.13, strand.y));
    float tone = mix(0.94, 1.06, yarn_seed)
        * (1.0 + (bundles.x - 0.5) * 0.34 * bundle_filter
            + (fibres.x - 0.5) * 0.22 * fibre_filter + (nap.x - 0.5) * 0.16 * pile * nap_filter
            + (filaments.x - filaments.y) * (0.27 + pile * 0.16));
    float shade = mix(1.0, lighting * tuck * edge_occlusion, relief) * tone;
    float accent_mix = knit_accent_mix(yarn_column, yarn_row);
    vec4 yarn = mix(base_color, premul_rect(knit_accent_color), accent_mix);
    vec4 ground = mix(base_color, yarn, 0.75);
    ground.rgb *= mix(0.68, 0.42, relief);
    yarn.rgb = min(yarn.rgb * shade, vec3(yarn.a));
    vec4 fabric = mix(ground, yarn, coverage);

    // A compact, uneven nap is intrinsic to wool, including at fuzz = 0.
    // Fuzz extends these short fibres; it does not blur the whole stitch.
    float halo_width = 0.018 + pile * 0.065;
    float halo = (1.0 - smoothstep(0.0, halo_width, max(distance, 0.0)))
        * (1.0 - coverage) * (0.08 + filaments.x * (0.65 + pile * 0.45));
    vec4 fibre_color = yarn;
    fibre_color.rgb = min(fibre_color.rgb * (1.06 + shoulder * 0.10), vec3(fibre_color.a));
    fabric = mix(fabric, fibre_color, halo);

    // Break only the subpixel cut edge inside the existing border geometry.
    float edge = min(frame.z, border_width - frame.z);
    float edge_pile = (0.25 + pile * 0.65) / max(niri_scale, 0.001);
    float silhouette = smoothstep(-edge_pile, edge_pile, edge
        + (bundles.x - 0.5) * edge_pile * 1.4);
    fabric *= silhouette;
    // Subpixel stitches converge to the material average instead of a moire grid.
    float resolved = smoothstep(1.0, 3.5, stitch_width * niri_scale);
    vec4 average = mix(base_color, premul_rect(knit_accent_color),
        knit_accent_mix(yarn_column, base_row));
    average.rgb *= mix(0.85, 0.72, relief);
    return mix(average, fabric, resolved);
}

vec4 knit_color(vec2 geometry_coords, vec4 base_color) {
    // Independent border quads interpolate the same geometry with slightly
    // different roundoff. Keep surface ownership stable below 1/256 physical px.
    float subpixel_grid = max(niri_scale, 0.001) * 256.0;
    geometry_coords = floor(geometry_coords * subpixel_grid + 0.5) / subpixel_grid;
    float stitch_width = max(knit_stitch_size, 1.0);
    KnitCourse motif = knit_course(min(border_width * 0.5, min(geo_size.x, geo_size.y) * 0.49), stitch_width);
    float period = 1.0;
    if (knit_pattern == 1.0)
        period = 3.0;
    else if (knit_pattern == 2.0)
        period = 4.0;
    else if (knit_pattern == 3.0 || knit_pattern == 4.0)
        period = 8.0;
    else if (knit_pattern == 5.0)
        period = 6.0;
    float count = dot(motif.edges + motif.bends, vec4(1.0));
    float closure = floor(count / period + 0.5) * period - count;
    if (motif.edges.x >= motif.edges.y)
        motif.edges.x = max(1.0, motif.edges.x + closure);
    else
        motif.edges.y = max(1.0, motif.edges.y + closure);
    return knit_fabric(geometry_coords, stitch_width, base_color, motif);
}
void main() {
    vec3 coords_geo = input_to_geo * vec3(niri_v_coords, 1.0);

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

    // Skip the gradient and knit pipelines for pixels outside the ring:
    // corner AA fringes and, for full-quad callers, the window interior.
    float alpha = ring_alpha * niri_alpha;
#if defined(DEBUG_FLAGS)
    // Tint mode visualizes the whole element quad; keep transparent pixels.
    if (alpha == 0.0 && niri_tint != 1.0)
        discard;
#else
    if (alpha == 0.0)
        discard;
#endif

    vec4 base_color = gradient_color(coords_geo.xy);
    vec4 color = knit_enabled == 1.0
        ? knit_color(coords_geo.xy, base_color)
        : base_color;

    color *= alpha;
#if defined(DEBUG_FLAGS)
    if (niri_tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
