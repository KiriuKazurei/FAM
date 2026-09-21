#ifndef FAMBRIDGE_H
#define FAMBRIDGE_H

#include <QJsonArray>
#include <QJsonValue>
#include <QString>
#include <QStringList>
#include <QVector>
#include <functional>

#include "famcore.h"

/* One row of a directory view, as decoded from a v2 buffer.
 *
 * Deliberately small: the path is not stored per row, because a view belongs to one directory and
 * composing the path on request is what keeps a 78k-row folder from carrying 78k duplicated
 * prefixes. The kind is the wire's index, resolved through the vocabulary the core reports at
 * startup rather than a string per row.
 */
struct FamViewRow {
    QString name;
    /* Set only for rows that came from a search, which spans directories so the path is not one the
     * caller can compose. A directory view leaves this empty and derives the path from its own
     * prefix - that is what keeps 78k rows from carrying 78k copies of it. */
    QString path;
    quint64 size = 0;
    qint64 modifiedMs = 0;
    quint8 kind = 0;
    bool directory = false;
};

/* A v2 header plus whatever the call that produced it put behind it. */
struct FamWindow {
    quint8 state = FAM_EMPTY;
    quint8 flags = 0;
    quint32 totalRows = 0;
    quint64 generation = 0;
    quint64 seq = 0;
    QVector<FamViewRow> rows;

    bool truncated() const { return (flags & FAM_FLAG_TRUNCATED) != 0; }
    bool resync() const { return (flags & FAM_FLAG_RESYNC) != 0; }
    bool lostEvents() const { return (flags & FAM_FLAG_LOSS) != 0; }
    /* A search sets this while it runs and at its ceiling, and never a directory view: a header that
     * carries it is telling the caller these rows are real but not all of them. */
    bool partial() const { return (flags & FAM_FLAG_INCOMPLETE) != 0; }
};

/* Owns a FamCore handle and turns the C ABI into Qt types.
 *
 * v1 calls return a QJsonValue built from the JSON payload; v2 calls decode a fixed-length buffer
 * into the structs above. The core is not thread safe, so this wrapper is only ever used from the
 * GUI thread - which the v2 directory calls make safe precisely because none of them touches the
 * filesystem: they read what the core's loader thread already put in the cache.
 */
class FamBridge {
public:
    /* An empty configPath makes the core fall back to the platform config file. */
    explicit FamBridge(const QString &configPath = QString(), QString *error = nullptr);
    ~FamBridge();

    FamBridge(const FamBridge &) = delete;
    FamBridge &operator=(const FamBridge &) = delete;

    bool healthy() const { return m_core != nullptr; }

    /* --- v2: open a directory without waiting, read it in windows ------------------------ */

    bool openDir(const QString &path, quint64 *token, QString *error);
    void closeDir(quint64 token);
    bool dirStatus(quint64 token, FamWindow *window, QString *error);
    bool dirFetch(quint64 token, quint32 first, quint32 count, FamWindow *window, QString *error);
    /* Positions to cut since the last call, in the order the model must apply them. */
    bool dirCuts(quint64 token, FamWindow *window, QVector<quint32> *cuts, QString *error);
    bool dirSetSort(quint64 token, const QString &sort, const QString &order, QString *error);
    bool dirSetFilter(quint64 token, const QJsonArray &terms, const QString &kind, QString *error);
    /* The kind names, in the order the wire's index means them. */
    bool coreKinds(QStringList *kinds, QString *error);

    /* --- v2: page a name search while it runs ---------------------------------------------
     *
     * A search token is read exactly like a directory token, with one difference: a record's name is
     * the whole path, which `searchPage` splits into the row's `path` and its display `name`.
     */
    bool beginSearch(const QString &query, const QString &within, const QString &kind,
                     quint32 limit, quint64 *token, QString *error);
    bool searchPage(quint64 token, quint32 first, quint32 count, FamWindow *window, QString *error);
    /* An empty page: the cheap way to ask where a search stands. */
    bool searchStatus(quint64 token, FamWindow *window, QString *error);
    void cancelSearch(quint64 token);
    void closeSearch(quint64 token);

    /* --- v1: JSON payloads, while the shell's tag half still uses them --------------------- */

    bool stats(QJsonValue *result, QString *error);
    bool tagAdd(const QString &path, const QStringList &tags, const QString &source, int priority,
                QJsonValue *result, QString *error);
    bool tagList(const QString &path, QJsonValue *result, QString *error);
    bool tagRemove(const QString &path, const QStringList &tags, QJsonValue *result, QString *error);
    bool find(const QStringList &tags, bool matchAll, const QString &within, QJsonValue *result,
              QString *error);

    /* The v1 view trio. Nothing in the shell calls these any more - it searches through v2 - but
     * `abi-smoke` does, because the claim it gates is that a v1 consumer still gets answers out of
     * this library, and going through the same wrapper the shell uses makes that claim stronger than
     * calling the C entry points raw would. */
    bool createView(const QString &sort, const QString &order, quint64 *view, QString *error);
    bool search(quint64 view, const QString &query, const QString &within, const QString &kind,
                QJsonValue *result, QString *error);
    bool drain(quint64 view, QJsonValue *result, QString *error);

    bool startWatch(const QString &dir, quint64 *watch, QString *error);

private:
    static QString take(char *raw);
    bool invoke(int code, char *out, char *err, QJsonValue *result, QString *error) const;
    /* The shared half of `dirFetch` and `searchPage`: both hand back a window of rows written into a
     * buffer this side owns, through the same C signature. */
    bool readRows(int (*call)(FamCore *, uint64_t, uint32_t, uint32_t, unsigned char *, uint32_t,
                              char **),
                  quint64 token, quint32 first, quint32 count, FamWindow *window,
                  QString *error) const;
    /* Reads a v2 header, and the records behind it when `rows` or `cuts` ask for them. */
    static bool decode(const unsigned char *buffer, int written, FamWindow *window,
                       QVector<quint32> *cuts);
    static QString nameOf(const unsigned char *buffer, int written, const FamRecord &record,
                          int blob);

    FamCore *m_core = nullptr;
};

#endif /* FAMBRIDGE_H */
