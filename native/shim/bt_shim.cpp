// Minimal C shim: one BlockingService + named models.
#include "bt_shim.h"

#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "translator/byte_array_util.h"
#include "translator/parser.h"
#include "translator/response.h"
#include "translator/response_options.h"
#include "translator/service.h"
#include "translator/utils.h"

namespace {
thread_local std::string g_last_error;
void set_error(const std::string &message) { g_last_error = message; }
}  // namespace

struct BtEngine {
  marian::bergamot::BlockingService service;
  std::map<std::string, std::shared_ptr<marian::bergamot::TranslationModel>> models;
  BtEngine() : service(marian::bergamot::BlockingService::Config{}) {}
};

extern "C" {
BtEngine *bt_init(void) {
  try {
    return new BtEngine();
  } catch (const std::exception &e) {
    set_error(e.what());
    return nullptr;
  } catch (...) {
    set_error("bt_init: unknown error");
    return nullptr;
  }
}

int bt_load(BtEngine *engine, const char *name, const char *model_config_path) {
  if (!engine || !name || !model_config_path) {
    set_error("bt_load: null argument");
    return -1;
  }
  try {
    marian::bergamot::TranslationModel::Config options =
        marian::bergamot::parseOptionsFromFilePath(model_config_path);
    auto model =
        std::make_shared<marian::bergamot::TranslationModel>(options, /*replicas=*/1);
    engine->models[name] = model;
    return 0;
  } catch (const std::exception &e) {
    set_error(e.what());
    return -1;
  } catch (...) {
    set_error("bt_load: unknown error");
    return -1;
  }
}

char *bt_translate(BtEngine *engine, const char *name, const char *text) {
  if (!engine || !name || !text) {
    set_error("bt_translate: null argument");
    return nullptr;
  }
  try {
    auto it = engine->models.find(name);
    if (it == engine->models.end()) {
      set_error(std::string("bt_translate: model not loaded: ") + name);
      return nullptr;
    }
    std::vector<std::string> sources;
    sources.emplace_back(text);
    std::vector<marian::bergamot::ResponseOptions> options = {
        marian::bergamot::ResponseOptions()};
    std::vector<marian::bergamot::Response> responses =
        engine->service.translateMultiple(it->second, std::move(sources), options);
    if (responses.empty()) {
      set_error("bt_translate: empty response");
      return nullptr;
    }
    const std::string &out = responses[0].target.text;
    char *p = (char *)malloc(out.size() + 1);
    if (!p) {
      set_error("bt_translate: out of memory");
      return nullptr;
    }
    memcpy(p, out.c_str(), out.size() + 1);
    return p;
  } catch (const std::exception &e) {
    set_error(e.what());
    return nullptr;
  } catch (...) {
    set_error("bt_translate: unknown error");
    return nullptr;
  }
}

void bt_free(char *s) { free(s); }

void bt_close(BtEngine *engine) { delete engine; }

const char *bt_last_error(void) { return g_last_error.c_str(); }
}
