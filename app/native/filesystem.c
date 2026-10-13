#include <node_api.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#include <sys/clonefile.h>
#include <sys/stat.h>
#include <sys/stdio.h>

static napi_value fail(napi_env env, int error, const char *operation, const char *error_code) {
    char message[256];
    snprintf(message, sizeof(message), "Package %s failed: %s (errno %d)", operation, strerror(error), error);
    napi_value text, code, value, number;
    if (napi_create_string_utf8(env, message, NAPI_AUTO_LENGTH, &text) != napi_ok
        || napi_create_string_utf8(env, error_code, NAPI_AUTO_LENGTH, &code) != napi_ok
        || napi_create_error(env, code, text, &value) != napi_ok
        || napi_create_int32(env, error, &number) != napi_ok
        || napi_set_named_property(env, value, "errno", number) != napi_ok) return NULL;
    napi_throw(env, value);
    return NULL;
}

static int descriptor(napi_env env, napi_value value, int *result) {
    double number;
    if (napi_get_value_double(env, value, &number) != napi_ok || !isfinite(number)
        || number < 0 || number > INT_MAX || number != (int)number) return 0;
    *result = (int)number;
    return 1;
}

static int filename(napi_env env, napi_value value, char *name) {
    size_t length = 0, copied = 0;
    return napi_get_value_string_utf8(env, value, NULL, 0, &length) == napi_ok
        && length > 0 && length <= NAME_MAX
        && napi_get_value_string_utf8(env, value, name, NAME_MAX + 1, &copied) == napi_ok
        && copied == length && memchr(name, '\0', length) == NULL
        && strchr(name, '/') == NULL && strcmp(name, ".") != 0 && strcmp(name, "..") != 0;
}

static napi_value clone_file(napi_env env, napi_callback_info info) {
    napi_value args[4];
    size_t count = 4;
    int source, directory;
    char name[NAME_MAX + 1];
    if (napi_get_cb_info(env, info, &count, args, NULL, NULL) != napi_ok || count != 3
        || !descriptor(env, args[0], &source) || !descriptor(env, args[1], &directory)
        || !filename(env, args[2], name)) {
        return fail(env, EINVAL, "clonefile", "ERR_PACKAGE_CLONE");
    }
    struct stat source_stat, directory_stat;
    if (fstat(source, &source_stat) != 0 || fstat(directory, &directory_stat) != 0) return fail(env, errno, "clonefile", "ERR_PACKAGE_CLONE");
    if (!S_ISREG(source_stat.st_mode) || !S_ISDIR(directory_stat.st_mode)) return fail(env, EINVAL, "clonefile", "ERR_PACKAGE_CLONE");
    if (fclonefileat(source, directory, name, CLONE_NOOWNERCOPY) != 0) return fail(env, errno, "clonefile", "ERR_PACKAGE_CLONE");
    napi_value result;
    if (napi_get_undefined(env, &result) != napi_ok) return NULL;
    return result;
}

static napi_value rename_directory_exclusive(napi_env env, napi_callback_info info) {
    napi_value args[5];
    size_t count = 5;
    int source, destination;
    char source_name[NAME_MAX + 1], destination_name[NAME_MAX + 1];
    if (napi_get_cb_info(env, info, &count, args, NULL, NULL) != napi_ok || count != 4
        || !descriptor(env, args[0], &source) || !filename(env, args[1], source_name)
        || !descriptor(env, args[2], &destination) || !filename(env, args[3], destination_name)) {
        return fail(env, EINVAL, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    struct stat source_parent, destination_parent, source_stat;
    if (fstat(source, &source_parent) != 0 || fstat(destination, &destination_parent) != 0) {
        return fail(env, errno, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    if (!S_ISDIR(source_parent.st_mode) || !S_ISDIR(destination_parent.st_mode)) {
        return fail(env, EINVAL, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    if (fstatat(source, source_name, &source_stat, AT_SYMLINK_NOFOLLOW) != 0) {
        return fail(env, errno, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    if (!S_ISDIR(source_stat.st_mode)) {
        return fail(env, EINVAL, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    if (renameatx_np(source, source_name, destination, destination_name, RENAME_EXCL) != 0) {
        return fail(env, errno, "exclusive directory rename", "ERR_PACKAGE_RENAME");
    }
    napi_value result;
    if (napi_get_undefined(env, &result) != napi_ok) return NULL;
    return result;
}

NAPI_MODULE_INIT() {
    napi_value function;
    if (napi_create_function(env, "cloneFile", NAPI_AUTO_LENGTH, clone_file, NULL, &function) != napi_ok
        || napi_set_named_property(env, exports, "cloneFile", function) != napi_ok) return NULL;
    if (napi_create_function(env, "renameDirectoryExclusive", NAPI_AUTO_LENGTH, rename_directory_exclusive, NULL, &function) != napi_ok
        || napi_set_named_property(env, exports, "renameDirectoryExclusive", function) != napi_ok) return NULL;
    return exports;
}
