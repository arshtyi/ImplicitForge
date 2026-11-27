// Storage/uniform members mirror the 16-byte lanes of Instruction and Params.
struct Instruction {
    op: vec4<u32>,
    center: vec4<f32>,
    size: vec4<f32>,
}
struct Params {
    dims: vec4<u32>,
    eye: vec4<f32>,
    forward: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    tint: vec4<f32>,
}
@group(0) @binding(0) var<uniform> frame: Params;
@group(0) @binding(1) var<storage, read> program: array<Instruction>;
@group(0) @binding(2) var<storage, read_write> pixels: array<u32>;

// The host validates every program before upload: no underflow, max stack 64,
// max length 255, exactly one final value. Binary ops pop rhs, then lhs.
fn scene(p: vec3<f32>) -> f32 {
    var stack: array<f32, 64>;
    var depth = 0u;
    for (var i = 0u; i < min(frame.dims.z, 255u); i++) {
        let inst = program[i];
        let op = inst.op.x;
        if op <= 2u {
            let q = p - inst.center.xyz;
            var d: f32;
            switch op {
                case 0u: { d = length(q) - inst.size.x; }
                case 1u: {
                    let e = abs(q) - inst.size.xyz;
                    d = length(max(e, vec3<f32>(0.0))) + min(max(e.x, max(e.y, e.z)), 0.0);
                }
                default: { d = length(vec2<f32>(length(q.xz) - inst.size.x, q.y)) - inst.size.y; }
            }
            stack[depth] = d;
            depth++;
        } else {
            depth--;
            let b = stack[depth];
            let a = stack[depth - 1u];
            var d = min(a, b);
            switch op {
                case 4u: { d = max(a, -b); }
                case 5u: { d = max(a, b); }
                case 6u: {
                    let k = inst.size.x;
                    if k > 0.0 {
                        let h = max(k - abs(a - b), 0.0) / k;
                        d -= h * h * k * 0.25;
                    }
                }
                default: {} // hard union
            }
            stack[depth - 1u] = d;
        }
    }
    return stack[0];
}

fn normalized_or_up(v: vec3<f32>) -> vec3<f32> {
    if dot(v, v) > 1e-12 { return normalize(v); }
    return vec3<f32>(0, 1, 0);
}

fn normal(p: vec3<f32>) -> vec3<f32> {
    let e = vec3<f32>(0.0015, 0, 0);
    return normalized_or_up(vec3<f32>(
        scene(p + e.xyz) - scene(p - e.xyz),
        scene(p + e.zxy) - scene(p - e.zxy),
        scene(p + e.yzx) - scene(p - e.yzx),
    ));
}

const LIGHT = vec3<f32>(-0.48, 0.8, 0.36);
fn shadow(origin: vec3<f32>) -> f32 {
    var v = 1.0;
    var s = 0.012;
    for (var i = 0u; i < 48u && s <= 12.0; i++) {
        let d = scene(origin + s * LIGHT);
        if d < 0.001 { return 0.0; }
        v = min(v, 12.0 * d / s);
        s += d;
    }
    return v;
}

fn trace(rd: vec3<f32>) -> vec3<f32> {
    let eye = frame.eye.xyz;
    let sky = mix(vec3<f32>(0.20, 0.30, 0.40), vec3<f32>(0.055, 0.095, 0.18),
                  clamp(rd.y * 1.8 + 0.18, 0.0, 1.0));
    var plane_t = 1e6;
    if rd.y < -0.0001 {
        let distance = (-1.2 - eye.y) / rd.y;
        if distance >= 0.0 { plane_t = distance; }
    }
    var t = 0.0;
    var object_hit = false;
    for (var i = 0u; i < 128u && t <= min(plane_t, 32.0); i++) {
        let d = abs(scene(eye + t * rd));
        if d < 0.001 { object_hit = true; break; }
        t += d;
    }
    if !object_hit {
        if plane_t > 32.0 { return sky; }
        t = plane_t;
    }
    let p = eye + rd * t;
    var n = vec3<f32>(0, 1, 0);
    var base: vec3<f32>;
    var rim = 0.0;
    if object_hit {
        n = normal(p);
        base = mix(frame.tint.xyz, vec3<f32>(0.95, 0.55, 0.21),
                   0.22 * clamp(p.y * 0.55 + 0.48, 0.0, 1.0));
        rim = pow(1.0 - clamp(dot(n, -rd), 0.0, 1.0), 3.0);
    } else {
        base = vec3<f32>(0.115, 0.165, 0.205) *
               (1.0 + 0.055 * cos(3.14159265 * p.x) * cos(3.14159265 * p.z));
    }
    let nl = max(dot(n, LIGHT), 0.0);
    var v = 1.0;
    if nl > 0.0 { v = shadow(p + 0.006 * n); }
    let h = normalized_or_up(LIGHT - rd);
    let spec = pow(max(dot(n, h), 0.0), 32.0) * v * nl;
    let color = base * ((0.30 + 0.10 * n.y) + 0.82 * nl * v) +
                vec3<f32>(0.95, 0.82, 0.65) * spec * select(0.08, 0.40, object_hit) +
                vec3<f32>(0.12, 0.22, 0.30) * rim;
    let fog = clamp((t - 6.0) / 26.0, 0.0, 1.0);
    return mix(color, sky, fog * fog);
}

fn sample_pixel(position: vec2<f32>) -> vec3<f32> {
    let uv = position * 2.0 / vec2<f32>(frame.dims.xy) - 1.0;
    return trace(normalize(frame.forward.xyz + uv.x * frame.right.xyz - uv.y * frame.up.xyz));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= frame.dims.xy) { return; }
    let origin = vec2<f32>(id.xy);
    var color = vec3<f32>(0.0);
    if frame.dims.w == 1u {
        color = sample_pixel(origin + vec2<f32>(0.5));
    } else {
        for (var y = 0u; y < 2u; y++) {
            for (var x = 0u; x < 2u; x++) {
                color += sample_pixel(origin + (vec2<f32>(f32(x), f32(y)) * 0.5 + 0.25));
            }
        }
        color *= 0.25;
    }
    let rgb = vec3<u32>(sqrt(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0))) * 255.0 + 0.5);
    pixels[id.y * frame.dims.x + id.x] = (rgb.r << 16u) | (rgb.g << 8u) | rgb.b;
}
