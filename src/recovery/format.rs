use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::app::AppState;

const MAGIC: [u8; 8] = *b"SINKREC\0";
const FORMAT_VERSION: u16 = 1;
const HEADER_SIZE: usize = 40;
const JOURNAL_LENGTH_SIZE: usize = size_of::<u64>();
const MAX_UNCOMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const CHECKPOINT_INTERVAL: usize = 32;
const ZSTD_LEVEL: i32 = 3;
const CHECKPOINT_FILE: &str = "checkpoint.sink";
const CHECKPOINT_TEMP_FILE: &str = "checkpoint.sink.tmp";
const JOURNAL_FILE: &str = "journal.sink";

/// envelope 中区分完整检查点和增量记录的稳定 tag。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum RecordKind {
    Checkpoint = 1,
    Delta = 2,
}

impl RecordKind {
    /// 从磁盘 tag 恢复受支持的记录类型。
    fn from_byte(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Checkpoint),
            2 => Ok(Self::Delta),
            _ => Err(format!("恢复记录类型 {value} 不受支持")),
        }
    }
}

/// 两份序列化状态之间只保存变化中段的 binary delta。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BinaryDelta {
    prefix_len: u64,
    removed_len: u64,
    replacement: Vec<u8>,
}

impl BinaryDelta {
    /// 计算 old 到 new 的最长公共前后缀和替换中段。
    fn between(old: &[u8], new: &[u8]) -> Self {
        let prefix_len = old
            .iter()
            .zip(new)
            .take_while(|(left, right)| left == right)
            .count();
        let suffix_limit = old.len().min(new.len()).saturating_sub(prefix_len);
        let suffix_len = old[old.len().saturating_sub(suffix_limit)..]
            .iter()
            .rev()
            .zip(new[new.len().saturating_sub(suffix_limit)..].iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        let old_end = old.len() - suffix_len;
        let new_end = new.len() - suffix_len;
        Self {
            prefix_len: prefix_len as u64,
            removed_len: (old_end - prefix_len) as u64,
            replacement: new[prefix_len..new_end].to_vec(),
        }
    }

    /// 在严格边界校验后把增量应用到上一份状态字节。
    fn apply(&self, old: &[u8]) -> Result<Vec<u8>, String> {
        let prefix_len = usize::try_from(self.prefix_len)
            .map_err(|_| "恢复增量 prefix 超出平台范围".to_owned())?;
        let removed_len = usize::try_from(self.removed_len)
            .map_err(|_| "恢复增量 removed length 超出平台范围".to_owned())?;
        let suffix_start = prefix_len
            .checked_add(removed_len)
            .filter(|end| *end <= old.len())
            .ok_or_else(|| "恢复增量引用了状态边界之外的字节".to_owned())?;
        let capacity = prefix_len
            .checked_add(self.replacement.len())
            .and_then(|length| length.checked_add(old.len() - suffix_start))
            .filter(|length| *length <= MAX_UNCOMPRESSED_BYTES)
            .ok_or_else(|| "恢复增量结果超过 64MB 上限".to_owned())?;
        let mut next = Vec::with_capacity(capacity);
        next.extend_from_slice(&old[..prefix_len]);
        next.extend_from_slice(&self.replacement);
        next.extend_from_slice(&old[suffix_start..]);
        Ok(next)
    }
}

/// 已完成完整 envelope 校验与解压的恢复记录。
struct DecodedRecord {
    kind: RecordKind,
    sequence: u64,
    payload: Vec<u8>,
}

/// 启动时从检查点与 journal 恢复出的状态和非致命诊断。
pub(super) struct LoadedRecovery {
    pub state: Option<AppState>,
    pub diagnostic: Option<String>,
}

/// 只在后台线程中访问的恢复文件状态机。
pub(super) struct RecoveryStore {
    directory: PathBuf,
    state_bytes: Option<Vec<u8>>,
    sequence: u64,
    deltas_since_checkpoint: usize,
}

impl RecoveryStore {
    /// 打开恢复目录，重放有效记录并修复截断或隔离损坏文件。
    pub fn open(directory: PathBuf) -> Result<(Self, LoadedRecovery), String> {
        fs::create_dir_all(&directory)
            .map_err(|error| format!("创建恢复目录 {} 失败: {error}", directory.display()))?;
        let mut store = Self {
            directory,
            state_bytes: None,
            sequence: 0,
            deltas_since_checkpoint: 0,
        };
        let diagnostic = store.load_records()?;
        let state = match store.state_bytes.as_deref() {
            Some(bytes) => match decode_state(bytes).and_then(|state| {
                state.validate_recovery()?;
                Ok(state)
            }) {
                Ok(state) => Some(state),
                Err(error) => {
                    let quarantine = store.quarantine_all()?;
                    store.state_bytes = None;
                    store.sequence = 0;
                    store.deltas_since_checkpoint = 0;
                    return Ok((
                        store,
                        LoadedRecovery {
                            state: None,
                            diagnostic: Some(format!(
                                "恢复状态无效，已隔离: {error}; {quarantine}"
                            )),
                        },
                    ));
                }
            },
            None => None,
        };
        Ok((store, LoadedRecovery { state, diagnostic }))
    }

    /// 序列化并持久化最新状态；相同字节不会产生重复记录。
    pub fn persist(&mut self, state: &AppState) -> Result<bool, String> {
        state.validate_recovery()?;
        let next_bytes = encode_state(state)?;
        if self.state_bytes.as_deref() == Some(next_bytes.as_slice()) {
            return Ok(false);
        }
        let next_sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "恢复记录序号耗尽".to_owned())?;
        if self.state_bytes.is_none() || self.deltas_since_checkpoint >= CHECKPOINT_INTERVAL {
            self.write_checkpoint(next_sequence, &next_bytes)?;
            self.deltas_since_checkpoint = 0;
        } else {
            let delta = BinaryDelta::between(
                self.state_bytes
                    .as_deref()
                    .expect("已有恢复状态应持有序列化字节"),
                &next_bytes,
            );
            let payload = encode_value(&delta)?;
            self.append_delta(next_sequence, &payload)?;
            self.deltas_since_checkpoint += 1;
        }
        self.state_bytes = Some(next_bytes);
        self.sequence = next_sequence;
        Ok(true)
    }

    /// 同步删除活动恢复文件；隔离文件保留供诊断。
    pub fn clean_active_files(&mut self) -> Result<(), String> {
        for path in [
            self.checkpoint_path(),
            self.checkpoint_temp_path(),
            self.journal_path(),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("删除恢复文件 {} 失败: {error}", path.display()));
                }
            }
        }
        self.state_bytes = None;
        self.sequence = 0;
        self.deltas_since_checkpoint = 0;
        Ok(())
    }

    /// 读取检查点和 journal；返回可展示的截断或隔离诊断。
    fn load_records(&mut self) -> Result<Option<String>, String> {
        let checkpoint_path = self.checkpoint_path();
        let journal_path = self.journal_path();
        if !checkpoint_path.exists() {
            if journal_path.exists() {
                let isolated = self.quarantine_paths(&[journal_path])?;
                return Ok(Some(format!("缺少恢复检查点，已隔离 journal: {isolated}")));
            }
            return Ok(None);
        }

        let checkpoint = match fs::read(&checkpoint_path)
            .map_err(|error| format!("读取 {} 失败: {error}", checkpoint_path.display()))
            .and_then(|bytes| decode_record(&bytes))
        {
            Ok(record) if record.kind == RecordKind::Checkpoint => record,
            Ok(_) => {
                let isolated = self.quarantine_all()?;
                return Ok(Some(format!("检查点记录类型无效，已隔离: {isolated}")));
            }
            Err(error) => {
                let isolated = self.quarantine_all()?;
                return Ok(Some(format!("检查点损坏，已隔离: {error}; {isolated}")));
            }
        };
        self.sequence = checkpoint.sequence;
        self.state_bytes = Some(checkpoint.payload);

        if !journal_path.exists() {
            return Ok(None);
        }
        let journal = fs::read(&journal_path)
            .map_err(|error| format!("读取 {} 失败: {error}", journal_path.display()))?;
        let mut cursor = 0;
        let mut valid_end = 0;
        let mut incomplete_tail = false;
        let mut corruption = None;
        while cursor < journal.len() {
            if journal.len() - cursor < JOURNAL_LENGTH_SIZE {
                incomplete_tail = true;
                break;
            }
            let length = u64::from_le_bytes(
                journal[cursor..cursor + JOURNAL_LENGTH_SIZE]
                    .try_into()
                    .expect("长度切片固定为八字节"),
            );
            cursor += JOURNAL_LENGTH_SIZE;
            let length = match usize::try_from(length) {
                Ok(length) if length <= HEADER_SIZE + MAX_COMPRESSED_BYTES => length,
                _ => {
                    corruption = Some("journal record 长度超过上限".to_owned());
                    break;
                }
            };
            let Some(end) = cursor.checked_add(length) else {
                corruption = Some("journal record 长度溢出".to_owned());
                break;
            };
            if end > journal.len() {
                incomplete_tail = true;
                break;
            }
            match self.apply_journal_record(&journal[cursor..end]) {
                Ok(()) => {
                    cursor = end;
                    valid_end = end;
                }
                Err(error) => {
                    corruption = Some(error);
                    break;
                }
            }
        }

        if let Some(error) = corruption {
            let isolated = self.quarantine_paths(&[journal_path])?;
            let bytes = self
                .state_bytes
                .clone()
                .ok_or_else(|| "journal 隔离后缺少有效恢复状态".to_owned())?;
            self.write_checkpoint(self.sequence, &bytes)?;
            self.deltas_since_checkpoint = 0;
            return Ok(Some(format!(
                "journal 损坏，已保留有效前缀并隔离: {error}; {isolated}"
            )));
        }
        if incomplete_tail {
            let file = OpenOptions::new()
                .write(true)
                .open(&journal_path)
                .map_err(|error| format!("打开截断 journal 失败: {error}"))?;
            file.set_len(valid_end as u64)
                .map_err(|error| format!("截断 journal 无效尾部失败: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("同步截断 journal 失败: {error}"))?;
            return Ok(Some("已忽略并截断崩溃产生的不完整 journal 尾部".to_owned()));
        }
        Ok(None)
    }

    /// 校验并应用一个完整 journal envelope。
    fn apply_journal_record(&mut self, bytes: &[u8]) -> Result<(), String> {
        let record = decode_record(bytes)?;
        if record.kind != RecordKind::Delta {
            return Err("journal 中出现非增量记录".to_owned());
        }
        if record.sequence <= self.sequence {
            return Ok(());
        }
        if record.sequence != self.sequence + 1 {
            return Err(format!(
                "journal 序号不连续: 期望 {}, 实际 {}",
                self.sequence + 1,
                record.sequence
            ));
        }
        let delta: BinaryDelta = decode_value(&record.payload)?;
        let next = delta.apply(
            self.state_bytes
                .as_deref()
                .ok_or_else(|| "增量记录缺少基础检查点".to_owned())?,
        )?;
        self.state_bytes = Some(next);
        self.sequence = record.sequence;
        self.deltas_since_checkpoint += 1;
        Ok(())
    }

    /// 原子写入完整检查点，成功替换后清空旧 journal。
    fn write_checkpoint(&self, sequence: u64, payload: &[u8]) -> Result<(), String> {
        let record = encode_record(RecordKind::Checkpoint, sequence, payload)?;
        let temporary_path = self.checkpoint_temp_path();
        let checkpoint_path = self.checkpoint_path();
        let mut file = File::create(&temporary_path)
            .map_err(|error| format!("创建 {} 失败: {error}", temporary_path.display()))?;
        file.write_all(&record)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("写入检查点临时文件失败: {error}"))?;
        fs::rename(&temporary_path, &checkpoint_path).map_err(|error| {
            format!("原子替换检查点 {} 失败: {error}", checkpoint_path.display())
        })?;
        let journal_path = self.journal_path();
        let journal = File::create(&journal_path)
            .map_err(|error| format!("截断 {} 失败: {error}", journal_path.display()))?;
        journal
            .sync_all()
            .map_err(|error| format!("同步空 journal 失败: {error}"))
    }

    /// 以长度前缀追加一个压缩增量并同步到磁盘。
    fn append_delta(&self, sequence: u64, payload: &[u8]) -> Result<(), String> {
        let record = encode_record(RecordKind::Delta, sequence, payload)?;
        let record_len =
            u64::try_from(record.len()).map_err(|_| "journal record 长度超出 u64".to_owned())?;
        let path = self.journal_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| format!("打开 {} 失败: {error}", path.display()))?;
        file.write_all(&record_len.to_le_bytes())
            .and_then(|()| file.write_all(&record))
            .and_then(|()| file.sync_data())
            .map_err(|error| format!("追加恢复增量失败: {error}"))
    }

    /// 隔离全部活动恢复文件并返回隔离结果描述。
    fn quarantine_all(&self) -> Result<String, String> {
        self.quarantine_paths(&[self.checkpoint_path(), self.journal_path()])
    }

    /// 把存在的损坏文件重命名为带时间戳的只读诊断副本。
    fn quarantine_paths(&self, paths: &[PathBuf]) -> Result<String, String> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut isolated = Vec::new();
        for path in paths {
            if !path.exists() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "恢复文件名不是有效 Unicode".to_owned())?;
            let target = path.with_file_name(format!("{file_name}.corrupt-{timestamp}"));
            fs::rename(path, &target).map_err(|error| {
                format!(
                    "隔离 {} 到 {} 失败: {error}",
                    path.display(),
                    target.display()
                )
            })?;
            isolated.push(target.display().to_string());
        }
        Ok(if isolated.is_empty() {
            "没有需要隔离的文件".to_owned()
        } else {
            isolated.join(", ")
        })
    }

    /// 返回完整检查点路径。
    fn checkpoint_path(&self) -> PathBuf {
        self.directory.join(CHECKPOINT_FILE)
    }

    /// 返回同目录原子替换使用的临时检查点路径。
    fn checkpoint_temp_path(&self) -> PathBuf {
        self.directory.join(CHECKPOINT_TEMP_FILE)
    }

    /// 返回 append-only journal 路径。
    fn journal_path(&self) -> PathBuf {
        self.directory.join(JOURNAL_FILE)
    }
}

/// 把 AppState 序列化为恢复格式的权威逻辑字节。
pub(super) fn encode_state(state: &AppState) -> Result<Vec<u8>, String> {
    encode_value(state)
}

/// 从完整逻辑字节恢复 AppState，并拒绝尾随数据。
fn decode_state(bytes: &[u8]) -> Result<AppState, String> {
    decode_value(bytes)
}

/// 使用统一 bincode 配置序列化格式 payload。
fn encode_value<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|error| format!("序列化恢复数据失败: {error}"))
}

/// 使用统一 bincode 配置解码并确认消费了完整 payload。
fn decode_value<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, String> {
    let (value, consumed) = bincode::serde::decode_from_slice(bytes, bincode::config::standard())
        .map_err(|error| format!("反序列化恢复数据失败: {error}"))?;
    if consumed != bytes.len() {
        return Err("恢复 payload 含有尾随数据".to_owned());
    }
    Ok(value)
}

/// 把未压缩 payload 编码为带固定头、CRC32 和 zstd 的记录。
fn encode_record(kind: RecordKind, sequence: u64, payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() > MAX_UNCOMPRESSED_BYTES {
        return Err("恢复 payload 超过 64MB 上限".to_owned());
    }
    let compressed = zstd::stream::encode_all(payload, ZSTD_LEVEL)
        .map_err(|error| format!("zstd 压缩恢复数据失败: {error}"))?;
    if compressed.len() > MAX_COMPRESSED_BYTES {
        return Err("压缩恢复 payload 超过 64MB 上限".to_owned());
    }
    let mut record = Vec::with_capacity(HEADER_SIZE + compressed.len());
    record.extend_from_slice(&MAGIC);
    record.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    record.push(kind as u8);
    record.push(0);
    record.extend_from_slice(&sequence.to_le_bytes());
    record.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    record.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    record.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    debug_assert_eq!(record.len(), HEADER_SIZE);
    record.extend_from_slice(&compressed);
    Ok(record)
}

/// 校验记录头、版本、长度、zstd payload 和 CRC32。
fn decode_record(record: &[u8]) -> Result<DecodedRecord, String> {
    if record.len() < HEADER_SIZE {
        return Err("恢复记录短于固定头".to_owned());
    }
    if record[..8] != MAGIC {
        return Err("恢复记录魔数不匹配".to_owned());
    }
    let version = u16::from_le_bytes(record[8..10].try_into().expect("版本固定为两字节"));
    if version != FORMAT_VERSION {
        return Err(format!(
            "恢复格式版本 {version} 不受支持，当前版本为 {FORMAT_VERSION}"
        ));
    }
    let kind = RecordKind::from_byte(record[10])?;
    let sequence = u64::from_le_bytes(record[12..20].try_into().expect("序号固定为八字节"));
    let uncompressed_len = usize::try_from(u64::from_le_bytes(
        record[20..28].try_into().expect("原始长度固定为八字节"),
    ))
    .map_err(|_| "恢复原始长度超出平台范围".to_owned())?;
    let compressed_len = usize::try_from(u64::from_le_bytes(
        record[28..36].try_into().expect("压缩长度固定为八字节"),
    ))
    .map_err(|_| "恢复压缩长度超出平台范围".to_owned())?;
    let checksum = u32::from_le_bytes(record[36..40].try_into().expect("校验和固定为四字节"));
    if uncompressed_len > MAX_UNCOMPRESSED_BYTES || compressed_len > MAX_COMPRESSED_BYTES {
        return Err("恢复记录长度超过 64MB 上限".to_owned());
    }
    if record.len() != HEADER_SIZE + compressed_len {
        return Err("恢复记录压缩长度与文件大小不一致".to_owned());
    }
    let decoder = zstd::stream::read::Decoder::new(&record[HEADER_SIZE..])
        .map_err(|error| format!("创建 zstd 恢复 decoder 失败: {error}"))?;
    let mut payload = Vec::with_capacity(uncompressed_len);
    decoder
        .take((MAX_UNCOMPRESSED_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|error| format!("zstd 解压恢复数据失败: {error}"))?;
    if payload.len() != uncompressed_len {
        return Err("恢复记录解压长度不匹配".to_owned());
    }
    if crc32fast::hash(&payload) != checksum {
        return Err("恢复记录 CRC32 校验失败".to_owned());
    }
    Ok(DecodedRecord {
        kind,
        sequence,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::{PageInkEntry, PageInkStore, PageKey};

    /// 验证 binary delta 可处理追加、删除和中段替换。
    #[test]
    fn binary_delta_round_trips_changed_middle() {
        for (old, new) in [
            (
                b"prefix-old-suffix".as_slice(),
                b"prefix-new-suffix".as_slice(),
            ),
            (b"short".as_slice(), b"shorter-tail".as_slice()),
            (b"remove-tail".as_slice(), b"remove".as_slice()),
            (b"same".as_slice(), b"same".as_slice()),
        ] {
            assert_eq!(BinaryDelta::between(old, new).apply(old), Ok(new.to_vec()));
        }
    }

    /// 验证 envelope 同时保护版本、类型、长度、压缩和校验和。
    #[test]
    fn envelope_round_trips_and_rejects_corruption() {
        let encoded =
            encode_record(RecordKind::Checkpoint, 7, b"steady ink").expect("有效 payload 应编码");
        let decoded = decode_record(&encoded).expect("有效 envelope 应解码");
        assert_eq!(decoded.kind, RecordKind::Checkpoint);
        assert_eq!(decoded.sequence, 7);
        assert_eq!(decoded.payload, b"steady ink");

        let mut corrupted = encoded;
        let last = corrupted.last_mut().expect("记录应有压缩 payload");
        *last ^= 0x5a;
        assert!(decode_record(&corrupted).is_err());
    }

    /// 验证逐页存储不受插入顺序影响，保证恢复状态字节可重复生成。
    #[test]
    fn page_store_serialization_is_deterministic() {
        let first_key = PageKey::new(1).expect("测试页键有效");
        let second_key = PageKey::new(2).expect("测试页键有效");
        let mut forward = PageInkStore::new();
        forward.save(first_key, PageInkEntry::default());
        forward.save(second_key, PageInkEntry::default());
        let mut reverse = PageInkStore::new();
        reverse.save(second_key, PageInkEntry::default());
        reverse.save(first_key, PageInkEntry::default());

        assert_eq!(
            encode_value(&forward).expect("正序状态应序列化"),
            encode_value(&reverse).expect("逆序状态应序列化")
        );
    }
}
