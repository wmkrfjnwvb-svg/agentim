// Gives the client what only Unity knows: the main thread, pause and resume,
// and the network. Add it to a GameObject that lives as long as the client.
//
//   var runner = gameObject.AddComponent<AgentIMRunner>();
//   runner.Client = AgentIMClient.Create(new ClientOptions {
//       Url = "https://chat.example.com",
//       DeviceId = SystemInfo.deviceUniqueIdentifier,
//       StoreDir = System.IO.Path.Combine(Application.persistentDataPath, "agentim"),
//       TokenProvider = ct => MyBackend.GetTokenAsync(ct),
//       Context = System.Threading.SynchronizationContext.Current,   // the main thread
//   });

using UnityEngine;

namespace AgentIM.Client
{
    public sealed class AgentIMRunner : MonoBehaviour
    {
        public AgentIMClient? Client { get; set; }
        private NetworkReachability last = NetworkReachability.NotReachable;
        private float nextCheck;

        private void Update()
        {
            if (Client == null || Time.unscaledTime < nextCheck) return;
            nextCheck = Time.unscaledTime + 1f;
            // Unity has no change event: a change of reachability (Wi-Fi to
            // cellular, or none) is a new network for the core.
            var now = Application.internetReachability;
            if (now != last)
            {
                last = now;
                Client.SetNetwork(now != NetworkReachability.NotReachable);
            }
        }

        private void OnApplicationPause(bool paused) => Client?.SetForeground(!paused);

        private void OnApplicationFocus(bool focused)
        {
            if (focused) Client?.SetForeground(true);
        }

        private void OnDestroy()
        {
            Client?.Dispose();
            Client = null;
        }
    }
}
