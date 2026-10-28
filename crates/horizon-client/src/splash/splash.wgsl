// The startup splash (splash/gpu.rs): the liquid, a signed-distance field of
// smooth-min'd balls that blends into the game window's rounded box, and
// textured or plain quads for the fox, the text and the progress bar.
// Colours are sRGB values written as they are (a non-sRGB target), with
// premultiplied alpha.

struct Liquid {
    // Physical pixels per logical pixel, the target's size (physical).
    res: vec4<f32>,
    // x, y, radius (logical pixels); w unused.
    balls: array<vec4<f32>, 24>,
    // count, morph, window, smooth-min k
    params: vec4<f32>,
    // The window's centre and size (logical).
    rect: vec4<f32>,
    // edge colour, corner radius
    edge: vec4<f32>,
    deep: vec4<f32>,
};
@group(0) @binding(0) var<uniform> liquid: Liquid;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}
fn blob(p: vec2<f32>) -> f32 {
    var d = 1e5;
    let n = i32(liquid.params.x);
    for (var i = 0; i < 24; i = i + 1) {
        if (i >= n) { break; }
        let b = liquid.balls[i];
        d = smin(d, length(p - b.xy) - b.z, liquid.params.w);
    }
    return d;
}
fn rounded(p: vec2<f32>) -> f32 {
    let r = max(liquid.edge.w, 0.5);
    let q = abs(p - liquid.rect.xy) - liquid.rect.zw * 0.5 + r;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}
fn sdf(p: vec2<f32>) -> f32 {
    return mix(blob(p), rounded(p), liquid.params.y);
}

@fragment
fn fs_liquid(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = frag.xy / liquid.res.x;
    let d = sdf(p);
    if (d > 3.0) { return vec4<f32>(0.0); }
    let e = 1.5;
    let n2 = vec2<f32>(sdf(p + vec2<f32>(e, 0.0)) - sdf(p - vec2<f32>(e, 0.0)),
                       sdf(p + vec2<f32>(0.0, e)) - sdf(p - vec2<f32>(0.0, e)));
    // Deep inside, the field's gradient breaks up near the balls' centres:
    // the surface flattens toward the middle.
    let n = normalize(vec3<f32>(-n2 * smoothstep(-26.0, -8.0, d), 1.6));
    let depth = clamp(-d / 48.0, 0.0, 1.0);
    var col = mix(liquid.edge.rgb, liquid.deep.rgb, depth);
    let spec = pow(max(dot(n, normalize(vec3<f32>(-0.45, -0.75, 1.0))), 0.0), 28.0);
    let rim = smoothstep(-6.0, 0.0, d) * 0.35;
    col = col + vec3<f32>(spec * 0.85 + rim);
    // The game window's own dark face, with its title strip.
    let top = p.y < liquid.rect.y - liquid.rect.w * 0.5 + 30.0;
    let chrome = select(vec3<f32>(0.07, 0.11, 0.16), vec3<f32>(0.10, 0.15, 0.21), top);
    col = mix(col, chrome, liquid.params.z);
    let a = clamp(0.5 - d / 1.1, 0.0, 1.0) * mix(0.94, 1.0, liquid.params.z);
    return vec4<f32>(min(col, vec3<f32>(1.0)) * a, a);
}

struct View {
    // The target's size in logical pixels.
    size: vec4<f32>,
};
@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var nearest: sampler;

struct Quad {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

// Positions in logical pixels (y down) and a depth; uv < 0 is a plain colour.
@vertex
fn vs_quad(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> Quad {
    var out: Quad;
    out.pos = vec4<f32>(pos.x / view.size.x * 2.0 - 1.0, 1.0 - pos.y / view.size.y * 2.0, pos.z, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_quad(in: Quad) -> @location(0) vec4<f32> {
    var c = in.color;
    if (in.uv.x >= 0.0) {
        let t = textureSample(image, nearest, in.uv);
        if (t.a < 0.1) { discard; }
        c = vec4<f32>(t.rgb * c.rgb, t.a * c.a);
    }
    return vec4<f32>(c.rgb * c.a, c.a);
}

// The splash's own texture (premultiplied, sRGB values) to the window:
// as it is, with straight alpha, or over a backdrop on an opaque window;
// encoded for an sRGB surface (which encodes again on write).
struct Resolve {
    // mode (0 premultiplied, 1 straight, 2 opaque), sRGB surface
    mode: vec4<f32>,
    backdrop: vec4<f32>,
};
@group(0) @binding(0) var<uniform> resolve: Resolve;
@group(0) @binding(1) var scene: texture_2d<f32>;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_resolve(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(scene, vec2<i32>(frag.xy), 0);
    var rgb = c.rgb;
    var a = c.a;
    if (resolve.mode.x == 1.0) {
        rgb = rgb / max(a, 0.0001);
    } else if (resolve.mode.x == 2.0) {
        rgb = rgb + resolve.backdrop.rgb * (1.0 - a);
        a = 1.0;
    }
    if (resolve.mode.y == 1.0) {
        rgb = to_linear(rgb);
    }
    return vec4<f32>(rgb, a);
}
