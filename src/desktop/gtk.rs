use std::ffi::{c_char, c_int, c_uint, c_void};

pub type Object = *mut c_void;
pub type Callback = unsafe extern "C" fn();

#[link(name = "libgtk-4.so.1", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    pub fn gtk_init_check() -> c_int;
    pub fn gtk_builder_new_from_string(xml: *const c_char, length: isize) -> Object;
    pub fn gtk_builder_get_object(builder: Object, name: *const c_char) -> Object;
    pub fn gdk_display_get_default() -> Object;
    pub fn gtk_css_provider_new() -> Object;
    pub fn gtk_css_provider_load_from_data(provider: Object, data: *const c_char, length: isize);
    pub fn gtk_style_context_add_provider_for_display(display: Object, provider: Object, priority: c_uint);
    pub fn gtk_window_present(window: Object);
    pub fn gtk_widget_get_first_child(widget: Object) -> Object;
    pub fn gtk_widget_get_parent(widget: Object) -> Object;
    pub fn gtk_widget_set_sensitive(widget: Object, sensitive: c_int);
    pub fn gtk_widget_set_visible(widget: Object, visible: c_int);
    pub fn gtk_widget_set_hexpand(widget: Object, expand: c_int);
    pub fn gtk_widget_add_css_class(widget: Object, class: *const c_char);
    pub fn gtk_widget_set_margin_start(widget: Object, margin: c_int);
    pub fn gtk_widget_set_margin_end(widget: Object, margin: c_int);
    pub fn gtk_widget_set_margin_top(widget: Object, margin: c_int);
    pub fn gtk_widget_set_margin_bottom(widget: Object, margin: c_int);
    pub fn gtk_label_new(text: *const c_char) -> Object;
    pub fn gtk_label_set_text(label: Object, text: *const c_char);
    pub fn gtk_label_set_markup(label: Object, markup: *const c_char);
    pub fn gtk_label_set_xalign(label: Object, align: f32);
    pub fn gtk_label_set_wrap(label: Object, wrap: c_int);
    pub fn gtk_label_set_selectable(label: Object, selectable: c_int);
    pub fn gtk_label_set_max_width_chars(label: Object, width: c_int);
    pub fn gtk_list_box_append(list: Object, child: Object);
    pub fn gtk_list_box_remove(list: Object, child: Object);
    pub fn gtk_list_box_get_row_at_index(list: Object, index: c_int) -> Object;
    pub fn gtk_list_box_get_selected_row(list: Object) -> Object;
    pub fn gtk_list_box_select_row(list: Object, row: Object);
    pub fn gtk_list_box_row_get_index(row: Object) -> c_int;
    pub fn gtk_list_box_row_set_activatable(row: Object, active: c_int);
    pub fn gtk_spinner_set_spinning(spinner: Object, spinning: c_int);
    pub fn gtk_combo_box_text_append_text(combo: Object, text: *const c_char);
    pub fn gtk_combo_box_set_active(combo: Object, index: c_int);
    pub fn gtk_combo_box_get_active(combo: Object) -> c_int;
    pub fn gtk_notebook_set_current_page(notebook: Object, index: c_int);
    pub fn gtk_progress_bar_set_fraction(bar: Object, fraction: f64);
    pub fn gtk_text_view_get_buffer(view: Object) -> Object;
    pub fn gtk_text_buffer_set_text(buffer: Object, text: *const c_char, len: c_int);
    pub fn gtk_box_new(orientation: c_int, spacing: c_int) -> Object;
    pub fn gtk_box_append(container: Object, child: Object);
}

#[link(name = "libgobject-2.0.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    pub fn g_object_unref(object: Object);
    pub fn g_signal_connect_data(
        object: Object,
        signal: *const c_char,
        handler: Callback,
        data: Object,
        destroy: Option<unsafe extern "C" fn(Object, Object)>,
        flags: c_uint,
    ) -> std::ffi::c_ulong;
}

#[link(name = "libglib-2.0.so.0", kind = "dylib", modifiers = "+verbatim")]
extern "C" {
    pub fn g_set_application_name(name: *const c_char);
    pub fn g_main_loop_new(context: Object, running: c_int) -> Object;
    pub fn g_main_loop_run(main_loop: Object);
    pub fn g_main_loop_quit(main_loop: Object);
    pub fn g_main_loop_unref(main_loop: Object);
    pub fn g_timeout_add(interval: c_uint, callback: unsafe extern "C" fn(Object) -> c_int, data: Object) -> c_uint;
    pub fn g_source_remove(source: c_uint) -> c_int;
}
