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

static QByteArray tagsToJson(const QStringList &tags) {
    QJsonArray array;
    for (const QString &tag : tags) {
        array.append(tag);
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
    const QByteArray utf8Tags = tagsToJson(tags);
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
    const QByteArray utf8Tags = tags.isEmpty() ? QByteArray("[]") : tagsToJson(tags);
    char *out = nullptr;
    char *err = nullptr;
    const int code =
        fam_tag_remove(m_core, utf8Path.constData(), utf8Tags.constData(), nullptr, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::find(const QStringList &tags, bool matchAll, const QString &within,
                     QJsonValue *result, QString *error) {
    const QByteArray utf8Tags = tagsToJson(tags);
    const QByteArray utf8Within = within.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_find(m_core, utf8Tags.constData(), matchAll ? 1 : 0,
                              within.isEmpty() ? nullptr : utf8Within.constData(), nullptr, 0, &out,
                              &err);
    return invoke(code, out, err, result, error);
}

/* The three below are v1's JSON view, and `abi-smoke` is their only caller: the gate that proves a v1
 * consumer still gets answers out of this library goes through the same wrapper the shell does. */

bool FamBridge::createView(const QString &sort, const QString &order, quint64 *view,
                           QString *error) {
    const QByteArray utf8Sort = sort.toUtf8();
    const QByteArray utf8Order = order.toUtf8();
    char *err = nullptr;
    const quint64 token = fam_view_new(m_core, utf8Sort.constData(), utf8Order.constData(), &err);
    if (token == 0) {
        if (error != nullptr) {
            *error = take(err);
        } else if (err != nullptr) {
            fam_free_string(err);
        }
        return false;
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    *view = token;
    return true;
}

bool FamBridge::search(quint64 view, const QString &query, const QString &within,
                       const QString &kind, QJsonValue *result, QString *error) {
    const QByteArray utf8Query = query.toUtf8();
    const QByteArray utf8Within = within.toUtf8();
    const QByteArray utf8Kind = kind.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_view_search(m_core, view, utf8Query.constData(),
                                     within.isEmpty() ? nullptr : utf8Within.constData(),
                                     kind.isEmpty() ? nullptr : utf8Kind.constData(), 0, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::drain(quint64 view, QJsonValue *result, QString *error) {
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_view_drain(m_core, view, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::startWatch(const QString &dir, quint64 *watch, QString *error) {
    const QByteArray utf8Dir = dir.toUtf8();
    char *err = nullptr;
    const quint64 token = fam_watch_start(m_core, utf8Dir.constData(), &err);
    if (token == 0) {
        if (error != nullptr) {
            *error = take(err);
        } else if (err != nullptr) {
            fam_free_string(err);
        }
        return false;
    }
    if (err != nullptr) {
        fam_free_string(err);
    }
    *watch = token;
    return true;
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

bool FamBridge::coreKinds(QStringList *kinds, QString *error) {
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_backends_json(m_core, &out, &err);
    QJsonValue result;
    if (!invoke(code, out, err, &result, error)) {
        return false;
    }
    const QJsonArray list = result.toObject().value(QStringLiteral("kinds")).toArray();
    for (const QJsonValue &value : list) {
        kinds->append(value.toString());
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
