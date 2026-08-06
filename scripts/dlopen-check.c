/*
 * dlopen-check — ask the real dynamic linker to load a Logos plugin.
 *
 * A shared object that builds is not a shared object that loads. `nix build`
 * links the plugin against store paths that a portable bundle no longer has,
 * so the question this answers is the one the build cannot: on a machine with
 * no /nix/store, can ld.so map this file and bind every symbol it needs?
 *
 * RTLD_NOW is the whole point. Under the default lazy binding an unresolved
 * function symbol is not discovered until something calls it, which in a CI
 * job is never — the check would pass on a bundle that crashes the moment a
 * user presses the button. RTLD_NOW forces every relocation up front.
 *
 * The Qt entry points are looked up but deliberately not called. A Logos
 * module is a Qt plugin (Q_PLUGIN_METADATA), so QPluginLoader — the loader
 * the module host actually uses — needs one of these exported; a .so that
 * loads but exports none of them is not a plugin, it is just a library.
 * Instantiating it is the host's job, and that is the next step in CI.
 * Which metadata symbol exists depends on the Qt version (Qt 6.3 renamed
 * qt_plugin_query_metadata to …_v2), so any one of them satisfies this.
 */
#include <dlfcn.h>
#include <stdio.h>

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "usage: dlopen-check <path-to-shared-object>\n");
        return 2;
    }

    const char *path = argv[1];

    dlerror();
    void *handle = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (handle == NULL) {
        const char *err = dlerror();
        fprintf(stderr, "dlopen(RTLD_NOW) failed for %s: %s\n", path,
                err != NULL ? err : "unknown error");
        return 1;
    }
    printf("dlopen(RTLD_NOW) ok: %s\n", path);

    static const char *const entry_points[] = {
        "qt_plugin_instance",
        "qt_plugin_query_metadata_v2",
        "qt_plugin_query_metadata",
        NULL,
    };

    int found = 0;
    for (int i = 0; entry_points[i] != NULL; i++) {
        dlerror();
        void *sym = dlsym(handle, entry_points[i]);
        if (sym != NULL && dlerror() == NULL) {
            printf("  exports %s\n", entry_points[i]);
            found++;
        }
    }

    if (found == 0) {
        fprintf(stderr,
                "%s exports no Qt plugin entry point: QPluginLoader could not "
                "instantiate it\n",
                path);
        return 1;
    }

    /*
     * No dlclose. Unloading a Qt plugin runs its static destructors for no
     * benefit here — the process is about to exit and take the mapping with
     * it — and a crash in that teardown would be reported as a load failure,
     * which is the one thing this program must not get wrong.
     */
    return 0;
}
