// Images and files in React Native (docs/02-architecture.md §2.7), as on
// the web: the thumbnail at its proportions (at most 240 × 240), a viewer
// with the enlarged thumbnail, "View original" and "Download"; a file as
// a card with its type's icon, name and size; the upload's progress.

import { useState } from 'react';
import { ActivityIndicator, Image, Linking, Modal, Pressable, StyleSheet, Text, View } from 'react-native';
import type { Message } from '@agentimhq/client-web/view';
import { fileType, fitBox, formatBytes, shortName, useClient, useFileUrl, type Strings } from '@agentimhq/react/hooks';
import { useTheme } from './theme';
import { file as fileColors } from './tokens';

const LABEL: Record<string, string> = { pdf: 'PDF', doc: 'DOC', sheet: 'XLS', slides: 'PPT', archive: 'ZIP', audio: '♪', video: '▶', code: '</>', image: 'IMG', other: 'FILE' };

/** A page with a folded corner in the type's colour, its label in white. */
export function FileIcon({ type, size = 40 }: { type: string; size?: number }) {
  const color = (fileColors as Record<string, string>)[type] ?? fileColors.other;
  const w = Math.round(size * 0.78);
  const fold = Math.round(size * 0.26);
  return (
    <View style={{ width: w, height: size }} accessibilityElementsHidden importantForAccessibility="no-hide-descendants">
      <View style={{ position: 'absolute', inset: 0, backgroundColor: color, borderRadius: 4, borderTopRightRadius: fold }} />
      <View style={{ position: 'absolute', top: 0, right: 0, width: fold, height: fold, backgroundColor: 'rgba(255,255,255,0.35)', borderBottomLeftRadius: 3 }} />
      <Text style={{ position: 'absolute', bottom: Math.round(size * 0.14), left: 0, right: 0, textAlign: 'center', color: '#fff', fontWeight: '700', fontSize: Math.round(size * 0.24) }}>{LABEL[type] ?? LABEL.other}</Text>
    </View>
  );
}

function percent(m: Message): number | undefined {
  const u = (m as any).upload;
  if (!u || m.status !== 'uploading' || !u.size) return undefined;
  return Math.min(100, Math.floor((u.sent / u.size) * 100));
}

function Bar({ pct, tone }: { pct: number; tone: string }) {
  return (
    <View style={styles.bar} accessibilityRole="progressbar" accessibilityValue={{ min: 0, max: 100, now: pct }}>
      <View style={[styles.barFill, { width: `${pct}%`, backgroundColor: tone }]} />
    </View>
  );
}

/**
 * The full-screen viewer of an image: the enlarged thumbnail, View original, Download, close
 * (Android back closes it too).
 */
export function ImageViewer({ convId, message: m, thumb, strings, onClose }: { convId: string; message: Message; thumb?: string; strings: Strings; onClose(): void }) {
  const client = useClient();
  const p = m.payload ?? {};
  const [original, setOriginal] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const url = async () => client.localPreview?.(p.local_source) ?? (await client.fileUrl!(convId, p.file_id, 'original')).url;
  const viewOriginal = async () => {
    setLoading(true);
    setFailed(false);
    try {
      const u = await url();
      // Loaded into the image cache first, so the swap does not flash.
      await Promise.resolve()
        .then(() => Image.prefetch?.(u))
        .catch(() => true);
      setOriginal(u);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  };
  return (
    <Modal visible transparent animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <View style={styles.viewer} accessibilityViewIsModal>
        <Pressable style={StyleSheet.absoluteFill} onPress={onClose} accessibilityLabel={strings.close} />
        <Image source={{ uri: original ?? thumb }} style={styles.viewerImage} resizeMode="contain" accessibilityLabel={p.caption || p.name} testID={original ? 'viewer-original' : 'viewer-thumb'} />
        {failed ? <Text style={styles.viewerCaption}>{strings.fileUnavailable}</Text> : p.caption ? <Text style={styles.viewerCaption}>{p.caption}</Text> : null}
        <View style={styles.viewerBar}>
          {!original ? (
            <Pressable onPress={() => void viewOriginal()} disabled={loading} accessibilityRole="button" style={styles.viewerButton}>
              {loading ? <ActivityIndicator color="#fff" /> : <Text style={styles.viewerText}>{strings.viewOriginal}</Text>}
            </Pressable>
          ) : null}
          <Pressable onPress={() => void url().then((u) => Linking.openURL(u))} accessibilityRole="button" accessibilityLabel={strings.download} style={styles.viewerButton}>
            <Text style={styles.viewerText}>↓</Text>
          </Pressable>
          <Pressable onPress={onClose} accessibilityRole="button" accessibilityLabel={strings.close} style={styles.viewerButton}>
            <Text style={styles.viewerText}>✕</Text>
          </Pressable>
        </View>
      </View>
    </Modal>
  );
}

/**
 * An image message: the thumbnail at its proportions (at most 240 × 240) with its caption and
 * upload progress; pressing opens the viewer.
 */
export function ImageMessage({ convId, message: m, strings, mine }: { convId: string; message: Message; strings: Strings; mine: boolean }) {
  const t = useTheme();
  const p = m.payload ?? {};
  const { url, error } = useFileUrl(convId, p, 'thumb', { blob: false });
  const [open, setOpen] = useState(false);
  const box = fitBox(p.width, p.height);
  const pct = percent(m);
  return (
    <View>
      <Pressable
        onPress={() => setOpen(true)}
        disabled={!url || !!error}
        accessibilityRole="imagebutton"
        accessibilityLabel={p.caption ? `${strings.image}: ${p.caption}` : strings.image}
        style={[styles.image, { width: box.width, height: box.height, backgroundColor: t.colors.surfaceHover }]}
        testID={`image-${m.key}`}
      >
        {url && !error ? <Image source={{ uri: url }} style={StyleSheet.absoluteFill} resizeMode="cover" /> : null}
        {error ? <Text style={[styles.gone, { color: t.colors.muted }]}>{strings.fileUnavailable}</Text> : null}
        {pct !== undefined ? (
          <View style={styles.imageProgress} accessibilityLabel={strings.uploading(pct)}>
            <ActivityIndicator color="#fff" />
            <Text style={styles.imageProgressText}>{pct}%</Text>
          </View>
        ) : null}
      </Pressable>
      {p.caption ? <Text style={[styles.caption, { color: mine ? t.colors.bubbleMineFg : t.colors.bubbleFg }]}>{p.caption}</Text> : null}
      {open ? <ImageViewer convId={convId} message={m} thumb={url} strings={strings} onClose={() => setOpen(false)} /> : null}
    </View>
  );
}

/**
 * A file message: a card with the type's icon, the name, the size and upload progress; pressing
 * opens the file.
 */
export function FileMessage({ convId, message: m, strings, mine }: { convId: string; message: Message; strings: Strings; mine: boolean }) {
  const client = useClient();
  const t = useTheme();
  const p = m.payload ?? {};
  const type = fileType(p.name, p.mime);
  const pct = percent(m);
  const [gone, setGone] = useState(false);
  const ready = !!p.file_id && m.status !== 'uploading';
  const fg = mine ? t.colors.bubbleMineFg : t.colors.bubbleFg;
  const open = async () => {
    try {
      const u = await client.fileUrl!(convId, p.file_id, 'original');
      await Linking.openURL(u.url);
    } catch {
      setGone(true);
    }
  };
  const detail = gone ? strings.fileUnavailable : pct !== undefined ? strings.uploading(pct) : `${formatBytes(p.size ?? 0)} · ${strings.fileTypes[type] ?? strings.fileTypes.other}`;
  return (
    <View>
      <Pressable onPress={() => void open()} disabled={!ready || gone} accessibilityRole="button" accessibilityLabel={`${p.name}, ${detail}`} style={({ pressed }) => [styles.file, pressed && { opacity: 0.7 }]} testID={`file-${m.key}`}>
        <FileIcon type={type} />
        <View style={styles.fileBody}>
          <Text style={[styles.fileName, { color: fg }]} numberOfLines={1}>
            {shortName(p.name ?? 'file')}
          </Text>
          <Text style={[styles.fileDetail, { color: fg }]}>{detail}</Text>
          {pct !== undefined ? <Bar pct={pct} tone={fg} /> : null}
        </View>
        {ready && !gone ? <Text style={[styles.fileGet, { color: fg }]}>↓</Text> : null}
      </Pressable>
      {p.caption ? <Text style={[styles.caption, { color: fg }]}>{p.caption}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create({
  image: { borderRadius: 14, overflow: 'hidden', justifyContent: 'center', alignItems: 'center' },
  gone: { fontSize: 13, textAlign: 'center', padding: 8 },
  imageProgress: { position: 'absolute', top: 0, left: 0, right: 0, bottom: 0, backgroundColor: 'rgba(0,0,0,0.35)', alignItems: 'center', justifyContent: 'center', gap: 4 },
  imageProgressText: { color: '#fff', fontSize: 12, fontWeight: '600' },
  caption: { fontSize: 15, lineHeight: 22, paddingHorizontal: 6, paddingTop: 4 },
  file: { flexDirection: 'row', alignItems: 'center', gap: 12, width: 240, maxWidth: '100%', minHeight: 44 },
  fileBody: { flex: 1, minWidth: 0, gap: 2 },
  fileName: { fontSize: 15, fontWeight: '600' },
  fileDetail: { fontSize: 13, opacity: 0.8 },
  fileGet: { fontSize: 18, opacity: 0.7 },
  bar: { height: 3, borderRadius: 2, backgroundColor: 'rgba(127,127,127,0.3)', overflow: 'hidden', marginTop: 2 },
  barFill: { height: 3 },
  viewer: { flex: 1, backgroundColor: 'rgba(9,9,11,0.94)', justifyContent: 'center', alignItems: 'center', paddingTop: 48, paddingBottom: 96 },
  viewerImage: { width: '100%', flex: 1 },
  viewerCaption: { color: '#fff', fontSize: 15, marginTop: 12, paddingHorizontal: 16, textAlign: 'center' },
  viewerBar: { position: 'absolute', bottom: 24, left: 0, right: 0, flexDirection: 'row', justifyContent: 'center', gap: 12 },
  viewerButton: { minWidth: 44, minHeight: 44, paddingHorizontal: 14, borderRadius: 22, backgroundColor: 'rgba(255,255,255,0.14)', alignItems: 'center', justifyContent: 'center' },
  viewerText: { color: '#fff', fontSize: 15 },
});
