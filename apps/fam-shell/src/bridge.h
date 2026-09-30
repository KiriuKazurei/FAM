#ifndef FAMBRIDGE_H
#define FAMBRIDGE_H

#include <QJsonArray>
#include <QJsonValue>
#include <QString>
#include <QStringList>
#include <QSet>
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
    /* The raw dwFileAttributes the enumeration recorded. Nothing in the shell reads it but the icon
     * layer, which needs the directory/offline bits to ask the shell's image list about a name
     * without opening it. */
    quint32 attrs = 0;
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

/* What the shell spent on the ABI. The self-check logs it because "a status poll is cheap" is a claim
 * about *this* side of the boundary: the core can answer a zero-count page in one header all it
 * likes, and a shell that retries with a four-times buffer every beat still pays two round trips.
 * That is exactly the bug the truncated flag caused once, and it is only visible by counting. */
struct FamIpcCount {
    int headers = 0; /* fam_dir_state, and a search page asked with count 0 */
    int pages = 0;   /* row-carrying calls that came back usable */
    int polls = 0;   /* fam_dir_poll */
    int retries = 0; /* extra attempts spent on a buffer that came back truncated */
    /* Crossings for the tags of a materialised window - the read the shell added because a row now
     * paints tags, and the one call on this surface whose cost is paid per beat rather than per
     * action. Counted separately because `total()` mixing it in would make a folder that grew by one
     * row look like it grew by a store read. */
    int tagReads = 0;
    /* The note marker's read: same shape, same beat, its own number - so "a folder that grew by one
     * row" and "a screen of note markers" stay distinguishable in the log. */
    int noteReads = 0;
    int total() const { return headers + pages + polls + retries + tagReads + noteReads; }
};

/* One tag name and how many files carry it, as the vocabulary read reports them - most used first.
 * The count is files, not rows: a name sitting on one file under both sources is one file wearing one
 * label, which is what a filter bar's number has to promise. */
struct TagFacet {
    QString tag;
    int files = 0;
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

    FamIpcCount ipcCount() const { return m_ipc; }

    /* --- v2: open a directory without waiting, read it in windows ------------------------ */

    bool openDir(const QString &path, quint64 *token, QString *error);
    void closeDir(quint64 token);
    bool dirStatus(quint64 token, FamWindow *window, QString *error);
    bool dirFetch(quint64 token, quint32 first, quint32 count, FamWindow *window, QString *error);
    /* Positions to cut since the last call, in the order the model must apply them. */
    bool dirCuts(quint64 token, FamWindow *window, QVector<quint32> *cuts, QString *error);
    bool dirSetSort(quint64 token, const QString &sort, const QString &order, QString *error);
    bool dirSetFilter(quint64 token, const QJsonArray &terms, const QString &kind, QString *error);
    /* What the core says about itself: the kind names in the order the wire's index means them, the
     * search backends it has, and which of those can answer right now. All three come from one call
     * because they are one JSON payload - and `available` is worth re-reading, since the index
     * backend's state is probed in the background and can change while the shell is open. */
    bool coreBackends(QStringList *kinds, QStringList *backends, QStringList *available,
                      QString *error);

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

    /* --- the half of the ABI that stayed JSON: control payloads, not rows --------------------- */

    bool stats(QJsonValue *result, QString *error);
    bool tagAdd(const QString &path, const QStringList &tags, const QString &source, int priority,
                QJsonValue *result, QString *error);
    bool tagList(const QString &path, QJsonValue *result, QString *error);
    bool tagRemove(const QString &path, const QStringList &tags, QJsonValue *result, QString *error);
    /* Renaming comes in two reaches, and both go through the store's one merge rule: a label on
     * this file only, and a label across the whole library - which is what consolidating a
     * duplicate (`pdf` beside `PDF`) is. */
    bool tagRename(const QString &path, const QString &from, const QString &to, QJsonValue *result,
                   QString *error);
    bool tagMerge(const QString &from, const QString &into, QJsonValue *result, QString *error);

    /* The free-text note beside a path: read, written whole, or dropped. A note is not a tag and
     * never appears in a listing or a search; the store's rules (8 KiB, nothing blank) come back
     * as their own sentences in `error`. */
    bool noteGet(const QString &path, QJsonValue *result, QString *error);
    bool noteSet(const QString &path, const QString &body, QJsonValue *result, QString *error);
    bool noteClear(const QString &path, QJsonValue *result, QString *error);

    /* --- the two reads a listing needs to show tags ------------------------------------------
     *
     * Both are answers about the library's state rather than acts, and neither is on the beat that
     * costs the most: `tagsForPaths` is what a column refreshes on its poll tick, and the core
     * treats it as a heartbeat, so it never reaches the audit trail. */

    /* The tags of the named paths, keyed by the caller's own spelling of each. A path that carries no
     * tag is absent from the result rather than present with an empty list, so "untagged" and "not
     * asked about" stay the same answer here as they are on the wire. */
    bool tagsForPaths(const QStringList &paths, QHash<QString, QStringList> *tags, QString *error);
    /* Which of the named paths carry a note - the keys of the answer, not the bodies. Absent means
     * "no note"; "not asked about" and "has no note" stay the same answer here as on the wire. */
    bool notesForPaths(const QStringList &paths, QSet<QString> *noted, QString *error);
    /* Every tag name in the store, most used first. */
    bool tagVocabulary(QVector<TagFacet> *facets, QString *error);

    /* Move `from` onto `to` - a full destination path, name included - or copy it when `copy` is
     * set. This is the first call on the bridge that changes what is on disk, so the rules that
     * refuse it (never overwrite, a directory is never copied, a cross-volume directory is refused)
     * are the ones in `famcore.h` beside `fam_path_op`, and `error` carries whichever one said no. */
    bool pathOp(const QString &from, const QString &to, bool copy, QJsonValue *result,
                QString *error);

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
    mutable FamIpcCount m_ipc;
};

#endif /* FAMBRIDGE_H */
