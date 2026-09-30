#ifndef FAMLISTMODEL_H
#define FAMLISTMODEL_H

#include <QAbstractListModel>
#include <QStringList>
#include <QSet>
#include <functional>

#include "bridge.h"
#include "rowdelegate.h"

class IconCache;

/* A listing painted from a v2 view without ever holding the whole thing: a directory, or a search's
 * rows as they arrive.
 *
 * The core reports how many rows a view holds; this model materialises one window of them, and
 * fetches a new window only when a paint asks for a row outside the one it holds. A 78k-row folder
 * therefore costs a viewport's worth of work per frame instead of 78k row allocations, and the fetch
 * that fills a miss is a bounded copy out of the core - no filesystem call, no wait.
 *
 * Change events arrive as positions to cut, in the order they must be applied. Consecutive equal
 * positions are a run of neighbours and become one beginRemoveRows, so deleting a thousand adjacent
 * files is one signal pair rather than a thousand.
 *
 * Icons are a join over data each row already carries - extension and attributes - never a field of
 * the row: the cache hands back a placeholder while a worker resolves the real glyph, and when a
 * batch lands only the window currently on screen is repainted, with DecorationRole named so that
 * delegates listening for text alone skip the signal entirely.
 */
class FamListModel final : public QAbstractListModel {
    Q_OBJECT

public:
    enum Roles {
        NameRole = Qt::UserRole + 1,
        KindRole,
        SizeRole,
        ModifiedRole,
        PathRole,
        DirectoryRole,
        /* A `QStringList` of the tags this row's path carries, empty when it carries none. Appended
         * last because the numbers already on the wire are what a delegate is built against. */
        TagsRole,
        /* `true` when the row's path carries a note, `false` otherwise. The same shape as tags: a
         * paint never asks the store, so this is a cache read driven by the poll beat. */
        HasNoteRole,
    };

    /* Fills `window` with rows [first, first + count) of the bound directory. Returns false when the
     * core could not answer; the model then paints nothing for that row rather than lying. */
    using Fetcher = std::function<bool(quint32 first, quint32 count, FamWindow *window)>;

    /* The tags of the named paths, keyed by the caller's own spelling of each - the bridge's
     * `tagsForPaths`, bound by whoever owns the ABI. A path with no tag is absent from `tags`. */
    using TagFetcher =
        std::function<bool(const QStringList &paths, QHash<QString, QStringList> *tags,
                           QString *error)>;

    /* The paths among a window that carry a note - the bridge's `notesForPaths`, bound by whoever
     * owns the ABI. A path with no note is absent from `noted`. */
    using NoteFetcher =
        std::function<bool(const QStringList &paths, QSet<QString> *noted, QString *error)>;

    explicit FamListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    /* --- dragging rows out -------------------------------------------------------------------
     *
     * A row of this model is a file, and its `mimeData` is the path the drop side will act on. The
     * payload is written by `FileView::mimeDataForPaths` so that the bytes a real drag carries and
     * the bytes a headless transfer step hands over are produced by one function. */

    QStringList mimeTypes() const override;
    QMimeData *mimeData(const QModelIndexList &indexes) const override;
    Qt::DropActions supportedDragActions() const override;

    /* `Qt::ItemIsDragEnabled`, which `QAbstractItemModel::flags()` does not set - and
     * `QAbstractItemView::canDrag()` refuses to start a drag without it, so this override is the line
     * that makes a row pickable at all. */
    Qt::ItemFlags flags(const QModelIndex &index) const override;

    void bind(quint64 token, const QString &directory, const QStringList &kinds, Fetcher fetch);
    void unbind();

    /* What the core says the view holds. Called from the status and fetch headers. */
    void readHead(const FamWindow &window);

    /* Cuts one poll reported. Returns the number of row-signal pairs emitted. */
    int applyCuts(const QVector<quint32> &positions);

    /* What a poll said when the rows were replaced by a re-read rather than patched. */
    struct Reconciled {
        /* Row-signal pairs spent on the change. */
        int pairs = 0;
        /* Rows announced as modified rather than as gone-and-arrived: the same item in the same
         * place, saying something different now. Zero pairs and a non-zero count here is the good
         * shape - a modification that costs a removal is the one that moves the reader's row. */
        int modified = 0;
        /* True when the window could not be reconciled and the view was rebuilt instead - which is
         * what costs the selection and the scroll position, so the caller logs it. */
        bool rebuilt = false;
    };

    /* Adopts a re-read's table without throwing the view away: the rows on screen are diffed against
     * the rows the core now has, and only what actually differs is announced. */
    Reconciled reconcile(const FamWindow &window);

    /* Where icons come from. Without one, DecorationRole answers empty and rows paint text-only. */
    void setIconCache(IconCache *cache);

    /* --- tags on the row ---------------------------------------------------------------------
     *
     * A row's tags are not a field of the row: they live in the store, and the listing protocol says
     * nothing about them. They arrive through this second seam, which only the poll beat drives -
     * `increment 7`'s rule that a paint must never ask a question of the filesystem or the store is
     * the reason there are two seams rather than one, and the reason `data()` reads a cache. */

    void setTagFetcher(TagFetcher fetch);

    /* Reads the tags of the window currently materialised and repaints the rows whose answer moved.
     * Cheap when nothing moved: the read is skipped outright when the window holds the same paths as
     * the last read, nothing is overdue, and nobody marked the cache dirty. Called from the beat, and
     * it bounds its own cost by answering at most once every `kTagRefreshBeats` beats unless one of
     * those three says otherwise - so an agent tagging files over MCP still reaches the screen, within
     * a second, without the shell paying a store read per column per 250ms for files nobody changed. */
    void refreshTags();

    /* Drops the skip: the next `refreshTags` reads even if the window is the same one. A tag edit
     * through this process's own dialog is the case - it changes no row, so nothing else would notice.
     * The cache is kept rather than cleared, so the chips on screen stay where they are for the one
     * beat it takes to get the true answer instead of flashing empty - which is the same lie for
     * 250ms either way, and the one that does not look like a broken row. */
    void invalidateTags();

    /* The notes of the named paths, keyed by the caller's own spelling of each - the bridge's
     * `notesForPaths`, bound by whoever owns the ABI. A path with no note is absent from `noted`. */
    void setNoteFetcher(NoteFetcher fetch);

    /* Reads the notes of the window currently materialised and repaints the rows whose answer moved.
     * The same skip the tags read takes: a window that has not moved, is not overdue, and was not
     * marked dirty costs nothing this beat. */
    void refreshNotes();

    /* Drops the skip: the next `refreshNotes` reads even if the window is the same one. A note edit
     * through this process's own dialog is the case - it changes no row, so nothing else would
     * notice. */
    void invalidateNotes();

    quint32 totalRows() const { return m_total; }
    QString pathAt(int index) const;
    int count() const { return int(m_total); }
    /* The row this path sits at, or -1. One lookup for the three callers that used to loop over
     * `pathAt` by hand, and the seam the headless steps read instead of the loop they kept
     * reinventing. */
    int findRowOfPath(const QString &path) const;

    /* Test seam: how many rows the model has materialised right now. */
    int materialised() const { return m_rows.size(); }

private slots:
    /* A batch of icons landed. Only the window currently materialised can be on screen, so that is
     * the range repainted - O(viewport), never O(rows) - and DecorationRole is named in the signal
     * so a delegate that only reads text skips it. */
    void iconsResolved();

private:
    const FamViewRow *rowAt(int index) const;
    bool fetchAt(int row) const;

    /* Two rows are the same row when everything a delegate paints about them is equal: identity
     * within the view plus size, time and kind. A file that was only *modified* is therefore not the
     * same row, and the diff spends a `dataChanged` on it instead of leaving a stale number on
     * screen - see `sameItem`, which is what the scan is aligned on. */
    static bool sameRow(const FamViewRow &left, const FamViewRow &right);
    /* Two rows are the same *item* when they are the same name in the same place, whatever it now
     * says about itself. That is the question the diff's prefix and suffix scans have to ask, because
     * a folder whose contents changed is the same item with a different size - and if the scan calls
     * that a row that left and a row that arrived, the view removes the reader's row and moves the
     * current index down to the next one. The column chain reads a moved current index as a person
     * having clicked it, and opens a folder nobody asked for. */
    static bool sameItem(const FamViewRow &left, const FamViewRow &right);

    QString m_directory;
    QStringList m_kinds;
    quint64 m_token = 0;
    Fetcher m_fetch;
    IconCache *m_icons = nullptr;
    TagFetcher m_tagFetch;
    /* The answer for the window that was materialised when it was read, keyed by the path spelling
     * this model handed to the seam. Replaced wholesale by each read, so it holds one window's worth
     * of tags and not one per row of a 78k-row folder. */
    QHash<QString, QStringList> m_tags;
    /* The paths the current `m_tags` was read for. Equal to the window's paths means the cache still
     * describes what is on screen; a cut, a scroll or a re-read that moved the window makes it differ,
     * which is the cheapest possible sign that the tags have to be asked about again. */
    QStringList m_taggedPaths;
    /* Counts down to the next scheduled read, and the flag that says somebody changed an answer behind
     * the cache's back. Both exist so a beat that changes nothing costs nothing. */
    int m_tagDue = 0;
    bool m_tagDirty = true;

    /* The notes seam: the same shape as the tags seam. `m_notes` is the set of paths that carry a
     * note, read for the window that was materialised when it was last asked. */
    NoteFetcher m_noteFetch;
    QSet<QString> m_notes;
    QStringList m_notedPaths;
    int m_noteDue = 0;
    bool m_noteDirty = true;

    /* Painting is what triggers a fetch, so the window it fills is mutable state - rows the core
     * already served, never guesses about rows it did not. */
    mutable quint32 m_total = 0;
    mutable int m_first = -1;
    mutable QVector<FamViewRow> m_rows;
    /* A search's rows arrive in discovery order and are sorted once, when it finishes, so the count is
     * not enough to know the view is unchanged: same number of rows, different order. */
    quint8 m_state = FAM_EMPTY;
    /* How many rows one fetch pulls. Fixed rather than asked of the view: the setter for this was
     * removed because nothing ever called it, and a window that is a screenful plus slack is what
     * the fetch budget was measured against. */
    int m_viewport = 128;
};

QString formatSize(quint64 bytes);

/* This model's roles, in the shape the shared row delegate reads. One definition rather than two
 * call sites inventing their own: the columns and the search results paint through the same
 * delegate, and a role list that disagrees between them is a row that shows a size in one place and
 * a kind in the other. */
inline const RowRoles kFamRowRoles{FamListModel::NameRole,    FamListModel::KindRole,
                                   FamListModel::SizeRole,    FamListModel::DirectoryRole,
                                   FamListModel::TagsRole,    FamListModel::HasNoteRole,
                                   &formatSize};

#endif /* FAMLISTMODEL_H */
