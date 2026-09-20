#ifndef FAMBRIDGE_H
#define FAMBRIDGE_H

#include <QJsonValue>
#include <QString>
#include <QStringList>

#include "famcore.h"

/* Owns a FamCore handle and turns the C ABI into Qt types.
 *
 * Every call returns a QJsonValue built from the JSON payload, or sets `error`. The core is not
 * thread safe, so this wrapper is only ever used from the GUI thread; watcher callbacks land in a
 * Rust-side queue that the GUI thread drains.
 */
class FamBridge {
public:
    /* An empty configPath makes the core fall back to the platform config file. */
    explicit FamBridge(const QString &configPath = QString(), QString *error = nullptr);
    ~FamBridge();

    FamBridge(const FamBridge &) = delete;
    FamBridge &operator=(const FamBridge &) = delete;

    bool healthy() const { return m_core != nullptr; }

    bool stats(QJsonValue *result, QString *error);
    bool tagAdd(const QString &path, const QStringList &tags, const QString &source, int priority,
                QJsonValue *result, QString *error);
    bool tagList(const QString &path, QJsonValue *result, QString *error);
    bool tagRemove(const QString &path, const QStringList &tags, QJsonValue *result, QString *error);
    bool find(const QStringList &tags, bool matchAll, const QString &within, QJsonValue *result,
              QString *error);

    bool createView(const QString &sort, const QString &order, quint64 *view, QString *error);
    bool search(quint64 view, const QString &query, const QString &within, const QString &kind,
                QJsonValue *result, QString *error);
    bool drain(quint64 view, QJsonValue *result, QString *error);
    bool startWatch(const QString &dir, quint64 *watch, QString *error);

private:
    static QString take(char *raw);
    bool invoke(int code, char *out, char *err, QJsonValue *result, QString *error) const;

    FamCore *m_core = nullptr;
};

#endif /* FAMBRIDGE_H */
