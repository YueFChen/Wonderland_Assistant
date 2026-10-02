// crypto.getRandomValues is available on HTTP LAN origins; randomUUID is HTTPS-only.
// Serve this before existing plugin bundles so their SDK works without rebuilding plugins.
if (typeof crypto.randomUUID !== 'function') {
  crypto.randomUUID = function () {
    var bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    var hex = Array.from(bytes, function (b) { return b.toString(16).padStart(2, '0'); }).join('');
    return hex.slice(0, 8) + '-' + hex.slice(8, 12) + '-' + hex.slice(12, 16) + '-' + hex.slice(16, 20) + '-' + hex.slice(20);
  };
}
