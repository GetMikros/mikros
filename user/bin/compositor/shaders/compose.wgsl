// Composing a frame: one quad for each layer, the wallpaper, a client's
// buffer or a panel's, drawn in order with premultiplied alpha blending.
// The fragment shader reads the layer's pixels from its image, XRGB8888
// or ARGB8888, and, inside the layer's haze area, puts them over the
// output's haze, or a flat colour, in the same draw (MIKSPEC-0066.R18);
// inside its blur area, over a translucent colour.

struct Layer {
    // Where the layer lies on the output, in pixels: x, y, width, height.
    rect: vec4<i32>,
    // The output's width and height.
    output: vec2<u32>,
    // 1 when the buffer's fourth byte is alpha.
    alpha: u32,
    // Under the haze area: 0 nothing, 1 the haze, 2 the flat colour.
    haze: u32,
    // The flat colour, XRGB8888, and the translucent colour under the
    // blur area, premultiplied ARGB8888.
    flat: u32,
    translucent: u32,
    // How much of the new haze shows while it fades in.
    fade: f32,
    // The haze area's and the blur area's rectangles in `areas`.
    haze_first: u32,
    haze_count: u32,
    blur_first: u32,
    blur_count: u32,
    // The part of the image that the layer shows, in texels: x, y, width
    // and height; a width of 0 is all of it.
    source: vec4<f32>,
}

@group(0) @binding(0) var pixels: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> haze_new: array<u32>;
@group(0) @binding(2) var<storage, read> haze_old: array<u32>;
@group(0) @binding(3) var<storage, read> areas: array<vec4<i32>>;
@group(0) @binding(4) var<storage, read> layers: array<Layer>;
@group(0) @binding(5) var stretch: sampler;

struct Corner {
    @builtin(position) at: vec4<f32>,
    @location(0) @interpolate(flat) layer: u32,
}

// The four corners of the layer's quad, as a triangle strip, in WGSL's
// clip space, whose y points up while the output's rows go down.
@vertex
fn vertex(@builtin(vertex_index) i: u32, @builtin(instance_index) layer: u32) -> Corner {
    let l = layers[layer];
    let corner = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    let rect = vec4<f32>(l.rect);
    let at = (rect.xy + corner * rect.zw) / vec2<f32>(l.output) * 2.0 - 1.0;
    return Corner(vec4<f32>(at.x, -at.y, 0.0, 1.0), layer);
}

// The red, green, blue and fourth byte of a pixel of a buffer of host
// memory, blue first in memory.
fn colour(pixel: u32) -> vec4<f32> {
    let c = unpack4x8unorm(pixel);
    return vec4<f32>(c.z, c.y, c.x, c.w);
}

fn inside(first: u32, count: u32, at: vec2<i32>) -> bool {
    for (var i = first; i < first + count; i++) {
        let r = areas[i];
        if all(at >= r.xy) && all(at < r.xy + r.zw) {
            return true;
        }
    }
    return false;
}

@fragment
fn fragment(corner: Corner) -> @location(0) vec4<f32> {
    let l = layers[corner.layer];
    // Clamped, so that every read stays inside the layer's buffer and the
    // haze.
    let on_output = clamp(vec2<i32>(corner.at.xy), vec2<i32>(0), vec2<i32>(l.output) - 1);
    let at = clamp(on_output - l.rect.xy, vec2<i32>(0), l.rect.zw - 1);
    // The pixels as they are when the layer has their size and shows all
    // of them; otherwise stretched to the layer, with bilinear filtering
    // (MIKSPEC-0074.R15).
    var c: vec4<f32>;
    let dimensions = vec2<f32>(textureDimensions(pixels));
    if l.source.z == 0.0 && all(l.rect.zw == vec2<i32>(textureDimensions(pixels))) {
        c = textureLoad(pixels, at, 0);
    } else {
        var uv = (vec2<f32>(at) + 0.5) / vec2<f32>(l.rect.zw);
        if l.source.z > 0.0 {
            // The part that the layer shows, whose edge texels filter with
            // themselves, not with those beside the part.
            let low = (l.source.xy + 0.5) / dimensions;
            let high = (l.source.xy + l.source.zw - 0.5) / dimensions;
            uv = min(max((l.source.xy + uv * l.source.zw) / dimensions, low), high);
        }
        c = textureSampleLevel(pixels, stretch, uv, 0.0);
    }
    if l.alpha == 0u {
        c.w = 1.0;
    }
    if l.haze != 0u && inside(l.haze_first, l.haze_count, at) {
        var under = colour(l.flat).xyz;
        if l.haze == 1u {
            let i = u32(on_output.y) * l.output.x + u32(on_output.x);
            under = mix(colour(haze_old[i]).xyz, colour(haze_new[i]).xyz, l.fade);
        }
        return vec4<f32>(c.xyz + (1.0 - c.w) * under, 1.0);
    }
    if inside(l.blur_first, l.blur_count, at) {
        let t = colour(l.translucent);
        return vec4<f32>(c.xyz + (1.0 - c.w) * t.xyz, c.w + (1.0 - c.w) * t.w);
    }
    return c;
}
