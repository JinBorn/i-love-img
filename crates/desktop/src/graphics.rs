//! Opt-in shader-cache diagnostics; normal startup does not compile extra shaders.
use eframe::glow::{self, HasContext};

pub fn initialize(cc: &eframe::CreationContext<'_>) {
    let Some(gl) = &cc.gl else {
        return;
    };
    let diagnostics = crate::diagnostics::Diagnostics::from_environment();
    if let Some(diagnostics) = &diagnostics
        && let Some(value) = std::env::var("LIT_DIAGNOSTIC_COLD_SHADER")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
    {
        let result = compile_probe(gl, value);
        diagnostics
            .log(serde_json::json!({"event":"cold-shader-probe","value":value,"result":result}));
    }
}

fn compile_probe(gl: &glow::Context, value: f32) -> Result<(), String> {
    // Diagnostic shaders use distinct actual output constants. They do not alter
    // application rendering or delete/replace any existing driver cache entries.
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        let result = (|| {
            for (kind, source) in [
                (
                    glow::VERTEX_SHADER,
                    "#version 140\nvoid main(){gl_Position=vec4(0.0,0.0,0.0,1.0);}".to_owned(),
                ),
                (
                    glow::FRAGMENT_SHADER,
                    format!(
                        "#version 140\nout vec4 color; void main(){{color=vec4({value:.9},0.2,0.3,1.0);}}"
                    ),
                ),
            ] {
                let shader = gl.create_shader(kind)?;
                shaders.push(shader);
                gl.shader_source(shader, &source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            Ok(())
        })();
        gl.delete_program(program);
        for shader in shaders {
            gl.delete_shader(shader);
        }
        result
    }
}
