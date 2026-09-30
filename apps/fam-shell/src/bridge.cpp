#include "bridge.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonParseError>
#include <QDebug>

#include <cstring>

FamBridge::FamBridge(const QString &configPath, QString *error) {
    char *err = nullptr;
    const QByteArray utf8Config = configPath.toUtf8();
    m_core = fam_core_new(configPath.isEmpty() ? nullptr : utf8Config.constData(), &err);
    if (m_core == nullptr && error != nullptr) {
        *error = take(err);
    } else if (err != nullptr) {
        fam_free_string(err);
    }
}

FamBridge::~FamBridge() {
    if (m_core != nullptr) {
        fam_core_free(m_core);
        m_core = nullptr;
    }
}

QString FamBridge::take(char *raw) {
    if (raw == nullptr) {
        return QString();
    }
    QString value = QString::fromUtf8(raw);
    fam_free_string(raw);
    return value;
}

/* Reads both out-params. Callers must invoke the C function on its own statement: C++ leaves
 * argument evaluation order unspecified, so passing `fam_stats(...)` and `out` in the same call can
 * read the pointer before the core has written it.
 */
bool FamBridge::invoke(int code, char *out, char *err, QJsonValue *result, QString *error) const {
    if (code != FAM_OK) {
        if (out != nullptr) {
            fam_free_string(out);
        }
        if (error != nullptr) {
            *error = take(err);
            if (error->isEmpty()) {
                *error = QStringLiteral("FAM core call failed without a message");
            }
        } else if (err != nullptr) {
            fam_free_string(err);
        }
        return false;
    }

    if (err != nullptr) {
        fam_free_string(err);
    }
    const QString payload = take(out);
    if (result == nullptr) {
        return true;
    }

    QJsonParseError parseError;
    const QJsonDocument document = QJsonDocument::fromJson(payload.toUtf8(), &parseError);
    if (parseError.error != QJsonParseError::NoError) {
        if (error != nullptr) {
            *error = QStringLiteral("core returned unparseable JSON: %1").arg(parseError.errorString());
        }
        return false;
    }
    *result = document.isArray() ? QJsonValue(document.array()) : QJsonValue(document.object());
    return true;
}

/* A JSON array of strings - the shape every tag and path list crosses the wire in.
 * Named for what it builds rather than for the first thing that needed it, because paths go
 * through the same encoder. */
static QByteArray stringArrayJson(const QStringList &values) {
    QJsonArray array;
    for (const QString &value : values) {
        array.append(value);
    }
    return QJsonDocument(array).toJson(QJsonDocument::Compact);
}

bool FamBridge::stats(QJsonValue *result, QString *error) {
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_stats(m_core, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagAdd(const QString &path, const QStringList &tags, const QString &source,
                       int priority, QJsonValue *result, QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    const QByteArray utf8Tags = stringArrayJson(tags);
    const QByteArray utf8Source = source.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_tag_add(m_core, utf8Path.constData(), utf8Tags.constData(),
                                 source.isEmpty() ? nullptr : utf8Source.constData(),
                                 static_cast<int32_t>(priority), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagList(const QString &path, QJsonValue *result, QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_tag_list(m_core, utf8Path.constData(), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagRemove(const QString &path, const QStringList &tags, QJsonValue *result,
                          QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    const QByteArray utf8Tags = tags.isEmpty() ? QByteArray("[]") : stringArrayJson(tags);
    char *out = nullptr;
    char *err = nullptr;
    const int code =
        fam_tag_remove(m_core, utf8Path.constData(), utf8Tags.constData(), nullptr, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagMerge(const QString &from, const QString &into, QJsonValue *result,
                         QString *error) {
    const QByteArray utf8From = from.toUtf8();
    const QByteArray utf8Into = into.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code =
        fam_tag_merge(m_core, utf8From.constData(), utf8Into.constData(), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagRename(const QString &path, const QString &from, const QString &to,
                          QJsonValue *result, QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    const QByteArray utf8From = from.toUtf8();
    const QByteArray utf8To = to.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_tag_rename(m_core, utf8Path.constData(), utf8From.constData(),
                                    to.isEmpty() ? nullptr : utf8To.constData(), nullptr, &out,
                                    &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::notesForPaths(const QStringList &paths, QSet<QString> *noted, QString *error) {
    noted->clear();
    if (paths.isEmpty()) {
        /* A window with nothing in it is a normal question with an empty answer, and it is not worth
         * crossing the wire to ask. */
        return true;
    }
    const QByteArray utf8Paths = stringArrayJson(paths);
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_notes_for_paths(m_core, utf8Paths.constData(), &out, &err);
    ++m_ipc.noteReads;
    QJsonValue result;
    if (!invoke(code, out, err, &result, error)) {
        return false;
    }
    const QJsonObject object = result.toObject();
    for (auto entry = object.constBegin(); entry != object.constEnd(); ++entry) {
        if (entry.value().toBool()) {
            noted->insert(entry.key());
        }
    }
    return true;
}

bool FamBridge::noteGet(const QString &path, QJsonValue *result, QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_note_get(m_core, utf8Path.constData(), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::noteSet(const QString &path, const QString &body, QJsonValue *result,
                        QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    const QByteArray utf8Body = body.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_note_set(m_core, utf8Path.constData(), utf8Body.constData(), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::noteClear(const QString &path, QJsonValue *result, QString *error) {
    const QByteArray utf8Path = path.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_note_clear(m_core, utf8Path.constData(), &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::tagsForPaths(const QStringList &paths, QHash<QString, QStringList> *tags,
                             QString *error) {
    tags->clear();
    if (paths.isEmpty()) {
        /* A window with nothing in it is a normal question with an empty answer, and it is not worth
         * crossing the wire to ask. */
        return true;
    }
    const QByteArray utf8Paths = stringArrayJson(paths);
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_tags_for_paths(m_core, utf8Paths.constData(), &out, &err);
    ++m_ipc.tagReads;
    QJsonValue result;
    if (!invoke(code, out, err, &result, error)) {
        return false;
    }
    const QJsonObject object = result.toObject();
    for (auto entry = object.constBegin(); entry != object.constEnd(); ++entry) {
        QStringList found;
        for (const QJsonValue &tag : entry.value().toArray()) {
            found.append(tag.toString());
        }
        tags->insert(entry.key(), found);
    }
    return true;
}

bool FamBridge::tagVocabulary(QVector<TagFacet> *facets, QString *error) {
    facets->clear();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_tag_vocabulary(m_core, &out, &err);
    QJsonValue result;
    if (!invoke(code, out, err, &result, error)) {
        return false;
    }
    for (const QJsonValue &value : result.toArray()) {
        const QJsonObject object = value.toObject();
        facets->append({object.value(QStringLiteral("tag")).toString(),
                        object.value(QStringLiteral("files")).toInt()});
    }
    return true;
}

bool FamBridge::pathOp(const QString &from, const QString &to, bool copy, QJsonValue *result,
                       QString *error) {
    const QByteArray utf8From = from.toUtf8();
    const QByteArray utf8To = to.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_path_op(m_core, utf8From.constData(), utf8To.constData(),
                                 copy ? FAM_PATH_COPY : 0u, &out, &err);
    return invoke(code, out, err, result, error);
}

/* --- v2: directory views ------------------------------------------------------------------- */

namespace {

/* 'FRAM', the first four bytes of any buffer the core wrote. */
constexpr uint32_t kMagic = 0x4D415246u;

/* A buffer is sized with this much name per row. A directory whose names are longer than the
 * allowance comes back truncated and gets one retry at four times the room. */
constexpr int kNameAllowance = 256;

int roomForRows(quint32 rows) {
    return int(sizeof(FamHead)) + int(rows) * (int(sizeof(FamRecord)) + kNameAllowance * 2);
}

/* The core and this shell are both x86_64 little-endian, and famcore.h asserts these sizes against
 * the Rust layout, so copying a struct out of the buffer is the whole deserialisation. */
template <typename T> T readAt(const unsigned char *buffer, int at) {
    T value;
    std::memcpy(&value, buffer + at, sizeof(T));
    return value;
}

/* Every v2 call that failed wrote its message the same way. */
bool report(char **err, QString *error) {
    if (error != nullptr) {
        *error = QString();
    }
    if (err != nullptr && *err != nullptr) {
        if (error != nullptr) {
            *error = QString::fromUtf8(*err);
        }
        fam_free_string(*err);
        *err = nullptr;
    }
    return false;
}

} // namespace

QString FamBridge::nameOf(const unsigned char *buffer, int written, const FamRecord &record,
                          int blob) {
    const qint64 start = qint64(blob) + qint64(record.name_off) * 2;
    const qint64 bytes = qint64(record.name_units) * 2;
    if (record.name_units == 0 || start < 0 || start + bytes > written) {
        /* A record pointing outside its own buffer is a version disagreement, not a row. */
        return QString();
    }
    return QString::fromUtf16(reinterpret_cast<const char16_t *>(buffer + start),
                              record.name_units);
}

bool FamBridge::decode(const unsigned char *buffer, int written, FamWindow *window,
                       QVector<quint32> *cuts) {
    if (written < int(sizeof(FamHead))) {
        return false;
    }
    const FamHead head = readAt<FamHead>(buffer, 0);
    if (head.magic != kMagic || head.abi != FAM_ABI_VERSION) {
        return false;
    }
    window->state = head.state;
    window->flags = head.flags;
    window->totalRows = head.total_rows;
    window->generation = head.generation;
    window->seq = head.seq;

    const int records = int(sizeof(FamHead));
    const int blob = records + int(head.row_count) * int(head.record_size);
    if (blob > written) {
        return false;
    }

    if (cuts != nullptr) {
        cuts->clear();
        if (head.record_size != sizeof(FamCut)) {
            return false;
        }
        cuts->reserve(int(head.row_count));
        for (uint32_t index = 0; index < head.row_count; ++index) {
            cuts->append(
                readAt<FamCut>(buffer, records + int(index) * int(sizeof(FamCut))).position);
        }
        return true;
    }

    if (head.row_count > 0 && head.record_size < sizeof(FamRecord)) {
        return false; /* too small a record to be the one this shell reads */
    }
    window->rows.clear();
    window->rows.reserve(int(head.row_count));
    for (uint32_t index = 0; index < head.row_count; ++index) {
        const FamRecord record =
            readAt<FamRecord>(buffer, records + int(index) * int(head.record_size));
        FamViewRow row;
        row.name = nameOf(buffer, written, record, blob);
        row.size = record.size;
        row.modifiedMs = record.modified_ms;
        row.kind = record.kind;
        row.directory = (record.flags & 0x1u) != 0;
        row.attrs = record.attrs;
        window->rows.append(row);
    }
    return true;
}

/* Two tries: the second with four times the room, for rows whose names are longer than the allowance.
 * A page that came back with nothing in it because the buffer was too small is not an answer the
 * caller can use, and past the retry the truncated flag is what they were going to get anyway. */
bool FamBridge::readRows(int (*call)(FamCore *, uint64_t, uint32_t, uint32_t, unsigned char *,
                                     uint32_t, char **),
                         quint64 token, quint32 first, quint32 count, FamWindow *window,
                         QString *error) const {
    for (int attempt = 0; attempt < 2; ++attempt) {
        if (attempt > 0) {
            ++m_ipc.retries;
        }
        QVector<unsigned char> buffer(roomForRows(count) * (attempt == 0 ? 1 : 4));
        char *err = nullptr;
        const int written = call(m_core, token, first, count, buffer.data(),
                                 uint32_t(buffer.size()), &err);
        if (written < 0) {
            return report(&err, error);
        }
        if (err != nullptr) {
            fam_free_string(err);
        }
        if (!decode(buffer.constData(), written, window, nullptr)) {
            if (error != nullptr) {
                *error = QStringLiteral("core sent a buffer this shell cannot read");
            }
            return false;
        }
        if (!window->truncated() || !window->rows.isEmpty() || attempt == 1) {
            if (count == 0) {
                ++m_ipc.headers;
            } else {
                ++m_ipc.pages;
            }
            return true;
        }
    }
    return true;
}

bool FamBridge::openDir(const QString &path, quint64 *token, QString *error) {
    const QByteArray utf8 = path.toUtf8();
    char *err = nullptr;
    const quint64 value = fam_open_dir(m_core, utf8.constData(), &err);
    if (value == 0) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    *token = value;
    return true;
}

void FamBridge::closeDir(quint64 token) {
    fam_dir_close(m_core, token);
}

bool FamBridge::dirStatus(quint64 token, FamWindow *window, QString *error) {
    ++m_ipc.headers;
    QVector<unsigned char> buffer(int(sizeof(FamHead)));
    char *err = nullptr;
    const int written = fam_dir_state(m_core, token, buffer.data(), uint32_t(buffer.size()), &err);
    if (written < 0) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    return decode(buffer.constData(), written, window, nullptr);
}

bool FamBridge::dirFetch(quint64 token, quint32 first, quint32 count, FamWindow *window,
                         QString *error) {
    return readRows(fam_dir_fetch, token, first, count, window, error);
}

bool FamBridge::dirCuts(quint64 token, FamWindow *window, QVector<quint32> *cuts, QString *error) {
    ++m_ipc.polls;
    /* Eight bytes a cut, so 64 KiB carries about 8000 of them and a bigger burst simply takes more
     * polls - the core keeps what did not fit rather than dropping it. */
    QVector<unsigned char> buffer(1 << 16);
    char *err = nullptr;
    const int written = fam_dir_poll(m_core, token, buffer.data(), uint32_t(buffer.size()), &err);
    if (written < 0) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    return decode(buffer.constData(), written, window, cuts);
}

bool FamBridge::dirSetSort(quint64 token, const QString &sort, const QString &order,
                           QString *error) {
    const QByteArray utf8Sort = sort.toUtf8();
    const QByteArray utf8Order = order.toUtf8();
    char *err = nullptr;
    if (fam_dir_set_sort(m_core, token, utf8Sort.constData(), utf8Order.constData(), &err)
        != FAM_OK) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    return true;
}

bool FamBridge::dirSetFilter(quint64 token, const QJsonArray &terms, const QString &kind,
                             QString *error) {
    const QByteArray utf8Terms = QJsonDocument(terms).toJson(QJsonDocument::Compact);
    const QByteArray utf8Kind = kind.toUtf8();
    char *err = nullptr;
    if (fam_dir_set_filter(m_core, token, utf8Terms.constData(),
                           kind.isEmpty() ? nullptr : utf8Kind.constData(), &err)
        != FAM_OK) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    return true;
}

bool FamBridge::coreBackends(QStringList *kinds, QStringList *backends, QStringList *available,
                             QString *error) {
    /* These are filled, not appended to, because the call is worth repeating: `available` changes
     * while the window is open, and a reader that appended would grow. */
    kinds->clear();
    backends->clear();
    available->clear();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_backends_json(m_core, &out, &err);
    QJsonValue result;
    if (!invoke(code, out, err, &result, error)) {
        return false;
    }
    const QJsonObject object = result.toObject();
    for (const QJsonValue &value : object.value(QStringLiteral("kinds")).toArray()) {
        kinds->append(value.toString());
    }
    for (const QJsonValue &value : object.value(QStringLiteral("backends")).toArray()) {
        backends->append(value.toString());
    }
    for (const QJsonValue &value : object.value(QStringLiteral("available")).toArray()) {
        available->append(value.toString());
    }
    return true;
}

/* --- v2: paged name search ------------------------------------------------------------------ */

bool FamBridge::beginSearch(const QString &query, const QString &within, const QString &kind,
                            quint32 limit, quint64 *token, QString *error) {
    const QByteArray utf8Query = query.toUtf8();
    const QByteArray utf8Within = within.toUtf8();
    const QByteArray utf8Kind = kind.toUtf8();
    char *err = nullptr;
    const quint64 value = fam_search_begin(m_core, utf8Query.constData(),
                                           within.isEmpty() ? nullptr : utf8Within.constData(),
                                           kind.isEmpty() ? nullptr : utf8Kind.constData(), limit,
                                           &err);
    if (value == 0) {
        return report(&err, error);
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    *token = value;
    return true;
}

bool FamBridge::searchPage(quint64 token, quint32 first, quint32 count, FamWindow *window,
                           QString *error) {
    if (!readRows(fam_search_page, token, first, count, window, error)) {
        return false;
    }
    /* A search row's name is its whole path, because a result set has no one directory to compose the
     * path from. The listing shows the last component and keeps the rest for opening. */
    for (FamViewRow &row : window->rows) {
        const int cut =
            qMax(row.name.lastIndexOf(QLatin1Char('/')), row.name.lastIndexOf(QLatin1Char('\\'))) + 1;
        row.path = row.name;
        row.name = row.name.mid(cut);
    }
    return true;
}

bool FamBridge::searchStatus(quint64 token, FamWindow *window, QString *error) {
    return searchPage(token, 0, 0, window, error);
}

void FamBridge::cancelSearch(quint64 token) {
    char *err = nullptr;
    if (fam_search_cancel(m_core, token, &err) < 0 && err != nullptr) {
        /* Nothing to do about it here: the only way this fails is a token this shell has already
         * dropped, and the search it refers to is over either way. */
        qWarning("fam_search_cancel: %s", qUtf8Printable(take(err)));
        return;
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
}

void FamBridge::closeSearch(quint64 token) { fam_search_close(m_core, token); }
