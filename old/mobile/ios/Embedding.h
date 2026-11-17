//
//  Embedding.h
//  Bridging header — declares the GPUI Rust FFI used by App.swift.
//

#ifndef EMBEDDING_H
#define EMBEDDING_H

void gpui_ios_register_app(void);
void gpui_ios_run_demo(void);
void gpui_ios_set_embedded(void);
void *gpui_ios_get_window(void);
void *gpui_ios_view_controller(void *window);
void gpui_ios_layout_view(void *window);
void gpui_ios_request_frame(void *window);
void gpui_ios_did_become_active(void *app);
void gpui_ios_will_resign_active(void *app);
void gpui_ios_handle_open_url(void *url_ptr);
void threadlane_mobile_did_become_active(void);

#endif /* EMBEDDING_H */
