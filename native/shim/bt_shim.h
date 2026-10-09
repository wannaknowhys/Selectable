// C ABI over Bergamot BlockingService (synchronous, worker-thread friendly).
// One engine (= one service/logger) holds many loaded models by name.
#pragma once
#if defined(_WIN32)
#if defined(BT_BUILDING_DLL)
#define BT_API __declspec(dllexport)
#else
#define BT_API __declspec(dllimport)
#endif
#else
#define BT_API
#endif
#ifdef __cplusplus
extern "C" {
#endif
typedef struct BtEngine BtEngine;
BT_API BtEngine *bt_init(void);
BT_API int bt_load(BtEngine *engine, const char *name, const char *model_config_path);
BT_API char *bt_translate(BtEngine *engine, const char *name, const char *text); /* malloc'd UTF-8 */
BT_API void bt_free(char *s);
BT_API void bt_close(BtEngine *engine);
/* Last error string (thread-local, valid until next call on this thread). */
BT_API const char *bt_last_error(void);
#ifdef __cplusplus
}
#endif
