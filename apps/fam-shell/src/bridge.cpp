#include "bridge.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonParseError>

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
    const int code = fam_tag_remove(m_core, utf8Path.constData(), utf8Tags.constData(), nullptr, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::find(const QStringList &tags, bool matchAll, const QString &within,
                     QJsonValue *result, QString *error) {
    const QByteArray utf8Tags = tagsToJson(tags);
    const QByteArray utf8Within = within.toUtf8();
    char *out = nullptr;
    char *err = nullptr;
    const int code = fam_find(m_core, utf8Tags.constData(), matchAll ? 1 : 0,
                              within.isEmpty() ? nullptr : utf8Within.constData(), nullptr, 0, &out, &err);
    return invoke(code, out, err, result, error);
}

bool FamBridge::createView(const QString &sort, const QString &order, quint64 *view, QString *error) {
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
