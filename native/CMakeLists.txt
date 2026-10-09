# Selectable translation build: upstream Bergamot (lib only) + our C shim.
# Invoked by `cargo xtask build-translate`, never by hand. All Windows-specific
# third-party handling (MKL paths, vcpkg toolchain) is passed in as -D flags.
cmake_minimum_required(VERSION 3.20)

project(selectable_translate CXX C)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(BERGAMOT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../third_party/bergamot-translator" CACHE PATH "bergamot-translator source")
set(MKL_DIR "" CACHE PATH "Extracted MKL static dir (include/ + lib/)")

if(NOT EXISTS "${BERGAMOT_DIR}/CMakeLists.txt")
  message(FATAL_ERROR "bergamot-translator submodule missing at ${BERGAMOT_DIR} (git submodule update --init)")
endif()

# Pin behavior to the proven spike recipe; keep upstream defaults otherwise.
set(COMPILE_TESTS OFF CACHE BOOL "" FORCE)
set(SSPLIT_USE_INTERNAL_PCRE2 OFF CACHE BOOL "" FORCE)
set(USE_STATIC_LIBS ON CACHE BOOL "" FORCE)
if(NOT DEFINED BUILD_ARCH)
  set(BUILD_ARCH "x86-64" CACHE STRING "")
endif()

if(MSVC)
  # /MD to match the Rust final link (ort-sys is /MD); see patches/.
  add_compile_options(/EHsc /MD /O2 /arch:SSE2 /MP /utf-8)
  add_compile_definitions(
    WIN32 _WINDOWS UNICODE _UNICODE
    _CRT_NONSTDC_NO_WARNINGS _CRT_SECURE_NO_WARNINGS
    BLAS_FOUND=1 COMPILE_CPU=1 MKL_FOUND=1 USE_INTGEMM=1 USE_PTHREADS USE_SSE2=1
    USE_SENTENCEPIECE _USE_INTERNAL_STRING_VIEW MKL_ILP64
  )
endif()

# MKL include for our TU (libs link by explicit path from Rust build.rs).
if(MKL_DIR)
  include_directories("${MKL_DIR}/include")
endif()

# Upstream tree, but excluded from ALL: we build only what bt_shim needs
# (app/ CLI and tests never compile).
add_subdirectory("${BERGAMOT_DIR}" bergamot-upstream EXCLUDE_FROM_ALL)

# Our engine as a DLL (translate_engine.dll): its bundled protobuf must stay
# isolated from ort's copy (static-linking both caused LNK2005 duplicates +
# heap corruption). C ABI keeps the boundary clean; shared /MD CRT keeps one
# heap for malloc/free across it.
add_library(translate_engine SHARED shim/bt_shim.cpp)
target_compile_definitions(translate_engine PRIVATE BT_BUILDING_DLL)
target_include_directories(translate_engine PRIVATE
  "${BERGAMOT_DIR}"
  "${BERGAMOT_DIR}/src"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party/SQLiteCpp/include"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party/sentencepiece"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party/fbgemm/include"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party/intgemm"
  "${BERGAMOT_DIR}/3rd_party/marian-dev/src/3rd_party/ruy"
  "${BERGAMOT_DIR}/3rd_party/ssplit-cpp/src/ssplit"
)
target_link_libraries(translate_engine PUBLIC bergamot-translator)
# MKL static trio goes INSIDE the DLL (BLAS/LAPACK symbols must resolve at
# DLL link time; nothing leaks to the exe dir).
if(MKL_DIR)
  target_link_libraries(translate_engine PRIVATE
    "${MKL_DIR}/lib/mkl_intel_ilp64.lib"
    "${MKL_DIR}/lib/mkl_sequential.lib"
    "${MKL_DIR}/lib/mkl_core.lib")
endif()
