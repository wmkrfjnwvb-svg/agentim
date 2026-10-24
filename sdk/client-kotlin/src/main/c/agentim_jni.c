/* JNI bridge from Kotlin (com.agentim.client.Native) to the C ABI
 * (sdk/client-rust/native/include/agentim.h). Strings cross as UTF-8 byte
 * arrays: JNI's own strings are "modified UTF-8" and break on emoji. The
 * SDK thread attaches to the JVM once and detaches when it exits (ART
 * aborts on a thread that exits attached). */

#include <jni.h>
#include <stdlib.h>
#include <string.h>
#ifndef _WIN32
#include <pthread.h>
#endif

#include "agentim.h"

static JavaVM *vm;
static jmethodID on_event;
#ifndef _WIN32
static pthread_key_t attached;

static void detach(void *unused) {
    (void)unused;
    (*vm)->DetachCurrentThread(vm);
}
#endif

typedef struct {
    agentim_client *client;
    jobject sink; /* global reference to a com.agentim.client.EventSink */
} handle;

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *jvm, void *reserved) {
    (void)reserved;
    vm = jvm;
#ifndef _WIN32
    pthread_key_create(&attached, detach);
#endif
    return JNI_VERSION_1_6;
}

static JNIEnv *env_here(int *detach_after) {
    JNIEnv *env = NULL;
    *detach_after = 0;
    if ((*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_6) == JNI_OK) return env;
    /* Android's jni.h takes JNIEnv**, the JDK's void**. */
#ifdef __ANDROID__
    if ((*vm)->AttachCurrentThreadAsDaemon(vm, &env, NULL) != JNI_OK) return NULL;
#else
    if ((*vm)->AttachCurrentThreadAsDaemon(vm, (void **)&env, NULL) != JNI_OK) return NULL;
#endif
#ifdef _WIN32
    *detach_after = 1;
#else
    pthread_setspecific(attached, (void *)1);
#endif
    return env;
}

static void callback(void *ctx, const char *json) {
    handle *h = ctx;
    int detach_after;
    JNIEnv *env = env_here(&detach_after);
    if (!env) return;
    jsize n = (jsize)strlen(json);
    jbyteArray b = (*env)->NewByteArray(env, n);
    if (b) {
        (*env)->SetByteArrayRegion(env, b, 0, n, (const jbyte *)json);
        (*env)->CallVoidMethod(env, h->sink, on_event, b);
        /* The sink catches its own errors; nothing is left pending here. */
        if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
        (*env)->DeleteLocalRef(env, b);
    }
#ifdef _WIN32
    if (detach_after) (*vm)->DetachCurrentThread(vm);
#else
    (void)detach_after;
#endif
}

/* A NUL-terminated copy of a byte array (NULL for null). */
static char *cstr(JNIEnv *env, jbyteArray a) {
    if (!a) return NULL;
    jsize n = (*env)->GetArrayLength(env, a);
    char *s = malloc((size_t)n + 1);
    if (!s) return NULL;
    (*env)->GetByteArrayRegion(env, a, 0, n, (jbyte *)s);
    s[n] = 0;
    return s;
}

JNIEXPORT jlong JNICALL Java_com_agentim_client_Native_create(JNIEnv *env, jclass cls, jbyteArray config,
                                                              jbyteArray store_dir, jobject sink) {
    (void)cls;
    if (!on_event) {
        /* The interface's method: valid for every implementation. */
        jclass k = (*env)->FindClass(env, "com/agentim/client/EventSink");
        if (!k) return 0;
        on_event = (*env)->GetMethodID(env, k, "onEvent", "([B)V");
        if (!on_event) return 0;
    }
    handle *h = calloc(1, sizeof *h);
    if (!h) return 0;
    h->sink = (*env)->NewGlobalRef(env, sink);
    char *c = cstr(env, config), *d = cstr(env, store_dir);
    h->client = c ? agentim_client_new(c, d, callback, h) : NULL;
    free(c);
    free(d);
    if (!h->client) {
        (*env)->DeleteGlobalRef(env, h->sink);
        free(h);
        return 0;
    }
    return (jlong)(intptr_t)h;
}

JNIEXPORT jint JNICALL Java_com_agentim_client_Native_command(JNIEnv *env, jclass cls, jlong p, jlong id, jbyteArray cmd) {
    (void)cls;
    char *s = cstr(env, cmd);
    int r = s ? agentim_client_command(((handle *)(intptr_t)p)->client, (uint64_t)id, s) : -1;
    free(s);
    return r;
}

JNIEXPORT void JNICALL Java_com_agentim_client_Native_token(JNIEnv *env, jclass cls, jlong p, jbyteArray token, jboolean refused) {
    (void)cls;
    char *s = cstr(env, token);
    agentim_client_token(((handle *)(intptr_t)p)->client, s, refused ? 1 : 0);
    free(s);
}

JNIEXPORT void JNICALL Java_com_agentim_client_Native_network(JNIEnv *env, jclass cls, jlong p, jboolean online, jlong generation) {
    (void)env;
    (void)cls;
    agentim_client_network(((handle *)(intptr_t)p)->client, online ? 1 : 0, (uint64_t)generation);
}

JNIEXPORT void JNICALL Java_com_agentim_client_Native_foreground(JNIEnv *env, jclass cls, jlong p, jboolean fg) {
    (void)env;
    (void)cls;
    agentim_client_foreground(((handle *)(intptr_t)p)->client, fg ? 1 : 0);
}

/* No event arrives after this returns; not to be called from an event. */
JNIEXPORT void JNICALL Java_com_agentim_client_Native_free(JNIEnv *env, jclass cls, jlong p) {
    (void)cls;
    handle *h = (handle *)(intptr_t)p;
    agentim_client_free(h->client);
    (*env)->DeleteGlobalRef(env, h->sink);
    free(h);
}

JNIEXPORT jstring JNICALL Java_com_agentim_client_Native_version(JNIEnv *env, jclass cls) {
    (void)cls;
    return (*env)->NewStringUTF(env, agentim_version());
}
