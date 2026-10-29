import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import Cogl from 'gi://Cogl';

// Multiply premultiplied RGBA by a soft edge, keeping actor geometry unchanged.
export const Reveal = GObject.registerClass(class LocalSendReveal extends Shell.GLSLEffect {
    _init() {
        this.progress = 1;
        super._init();
        this.location = this.get_uniform_location('ls_reveal');
        this.set_uniform_float(this.location, 1, [this.progress]);
    }

    vfunc_build_pipeline() {
        this.add_glsl_snippet(Cogl.SnippetHook.FRAGMENT, 'uniform float ls_reveal;', `
            float edge = 1.24 * ls_reveal;
            cogl_color_out *= 1.0 - smoothstep(edge - 0.24, edge, cogl_tex_coord_in[0].s);
        `, false);
    }

    setProgress(value) {
        this.progress = value;
        if (this.location !== undefined) this.set_uniform_float(this.location, 1, [value]);
        this.set_enabled(value < 1);
        this.queue_repaint();
    }
});
