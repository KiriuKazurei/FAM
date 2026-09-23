#ifndef FAMLISTMODEL_H
#define FAMLISTMODEL_H

#include <QAbstractListModel>
#include <QStringList>
#include <QVector>
#include <functional>

#include "bridge.h"

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
    };

    /* Fills `window` with rows [first, first + count) of the bound directory. Returns false when the
     * core could not answer; the model then paints nothing for that row rather than lying. */
    using Fetcher = std::function<bool(quint32 first, quint32 count, FamWindow *window)>;

    explicit FamListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

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
        /* True when the window could not be reconciled and the view was rebuilt instead - which is
         * what costs the selection and the scroll position, so the caller logs it. */
        bool rebuilt = false;
    };

    /* Adopts a re-read's table without throwing the view away: the rows on screen are diffed against
     * the rows the core now has, and only what actually differs is announced. */
    Reconciled reconcile(const FamWindow &window);

    /* Where icons come from. Without one, DecorationRole answers empty and rows paint text-only. */
    void setIconCache(IconCache *cache);

    quint32 totalRows() const { return m_total; }
    QString pathAt(int index) const;
    int count() const { return int(m_total); }

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
     * same row, and the diff spends a pair on it instead of leaving a stale number on screen. */
    static bool sameRow(const FamViewRow &left, const FamViewRow &right);

    QString m_directory;
    QStringList m_kinds;
    quint64 m_token = 0;
    Fetcher m_fetch;
    IconCache *m_icons = nullptr;

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

#endif /* FAMLISTMODEL_H */
