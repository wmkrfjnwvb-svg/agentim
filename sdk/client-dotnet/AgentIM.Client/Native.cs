// The C ABI of the Rust runtime (sdk/client-rust/native/include/agentim.h).
// Strings cross as NUL-terminated UTF-8; the callback runs on the SDK's own
// thread and its string is only valid during the call.

using System;
using System.Runtime.InteropServices;
using System.Text;

namespace AgentIM.Client
{
    internal static class Native
    {
#if UNITY_IOS && !UNITY_EDITOR
        // Linked statically into the app on iOS.
        private const string Lib = "__Internal";
#else
        private const string Lib = "agentim_client";
#endif

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        internal delegate void EventCallback(IntPtr ctx, IntPtr json);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern IntPtr agentim_client_new(byte[] config, byte[]? storeDir, EventCallback cb, IntPtr ctx);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int agentim_client_command(IntPtr c, ulong id, byte[] cmd);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void agentim_client_token(IntPtr c, byte[]? token, int refused);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void agentim_client_network(IntPtr c, int online, ulong generation);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void agentim_client_foreground(IntPtr c, int foreground);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void agentim_client_free(IntPtr c);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern IntPtr agentim_version();

        internal static byte[] Utf8(string s)
        {
            var n = Encoding.UTF8.GetByteCount(s);
            var b = new byte[n + 1];
            Encoding.UTF8.GetBytes(s, 0, s.Length, b, 0);
            return b;
        }

        internal static string FromUtf8(IntPtr p)
        {
            if (p == IntPtr.Zero) return "";
            var n = 0;
            while (Marshal.ReadByte(p, n) != 0) n++;
            var b = new byte[n];
            Marshal.Copy(p, b, 0, n);
            return Encoding.UTF8.GetString(b);
        }
    }
}
