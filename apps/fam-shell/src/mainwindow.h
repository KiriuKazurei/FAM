#ifndef MAINWINDOW_H
#define MAINWINDOW_H

#include <QElapsedTimer>
#include <QWidget>

#include <memory>

#include "bridge.h"
#include "columnchain.h"
#include "famlistmodel.h"

class IconCache;
class QComboBox;
class QLabel;
class QLineEdit;
class QListView;
class QStackedWidget;
class QTimer;

class MainWindow final : public QWidget {
    Q_OBJECT

public:
    /* `configPath` empty means the platform config file; `initialQuery` runs a search once shown. */
    explicit MainWindow(const QString &configPath = QString(),
                        const QString &initialQuery = QString(), QWidget *parent = nullptr);

    ~MainWindow() override;

    /* Shows one directory and keeps it current: the core watches what a view is open on, so opening
     * is the whole of the subscription. Returns false with `error` set on failure. */
    bool openFolder(const QString &dir, QString *error);

    IconCache *icons() const { return m_icons; }

    /* What the run spent on the ABI, for the headless gate to read. */
    FamIpcCount ipcCount() const {
        return m_bridge != nullptr ? m_bridge->ipcCount() : FamIpcCount();
    }

    /* Drives the deepest column through several viewports and lets the paints settle. The headless
     * gate uses it to make the icon worker touch every extension in a folder before the HICON
     * counters are read: a leak is only visible after the code path has actually run. */
    void scrollForSelfCheck();

    /* One file in, one file out, on the folder the self-check is watching. Creating it is what makes
     * the core's reconciled re-read happen while a view is open, which is the only way the shell's
     * reconcile path gets run in a headless gate - and it has to be driven from inside the process,
     * because a harness that creates the file from outside cannot tell whether the run is still
     * alive. Opt-in: nothing writes into a folder the user pointed at unless asked. */
    void createSelfCheckProbe();
    void removeSelfCheckProbe();

    /* The search box, driven from the headless timeline: starting a search while a folder is open,
     * and clearing the box to go back, are the two transitions a run with no hands on the keyboard
     * would otherwise never take. */
    void runSelfCheckSearch(const QString &terms);
    void clearSelfCheckSearch();

    /* Picks the first folder in the deepest column, which is what makes a second column appear. */
    void descendForSelfCheck();

    /* Clicks the deepest column's sort control once. */
    void sortDeepestForSelfCheck();

protected:
    /* Unhandled key events reach here: Backspace and Alt+Left go up one column. */
    void keyPressEvent(QKeyEvent *event) override;

private slots:
    void runSearch();
    void pollChanges();
    void openFolderDialog();
    void tagSelectedFile();
    /* Putting the folder back on screen: a search no longer closes the columns it was started in, so
     * clearing the box is what returns to them. */
    void showFolder();

private:
    void setStatus(const QString &text);
    /* One place words a directory header. The states are opposite claims that used to render the
     * same: a folder with nothing in it, a folder nobody has asked to read, and a folder that refuses
     * to be read (an ACL, a deleted path, a dead UNC share) all looked like a live, empty listing. */
    QString folderStatus(const QString &directory, const FamWindow &head,
                         const QString &extra = QString()) const;
    /* What a name search can answer with right now. Re-read rather than remembered: the index
     * backend's state is probed on the core's own thread and can change while the window is open. */
    QString searchBackendNote();
    /* One poll of a search that is still running: its row count grows, and when it stops growing the
     * rows are re-sorted, which is the last thing the model has to hear about. */
    void pollSearch();
    void showResults();
    void refreshScopeAvailability();
    QString deepestFolder() const { return m_chain->deepestPath(); }

    std::unique_ptr<FamBridge> m_bridge;
    /* The Miller columns. Owns one v2 token and one model per directory it shows. */
    ColumnChain *m_chain = nullptr;
    /* Search results get their own list and model rather than borrowing a column's: the columns stay
     * open and polled behind them, which is what makes returning a page flip instead of a re-read. */
    QListView *m_results = nullptr;
    FamListModel *m_resultsModel = nullptr;
    QStackedWidget *m_pages = nullptr;
    IconCache *m_icons = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    /* Whether a search is scoped to the folder on screen or spans every configured root. Without it,
     * "search here" would be the only thing a search could do once a folder was open - and the
     * previous build could only search everywhere, so either one alone is a regression. */
    QComboBox *m_scope = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    QStringList m_backends;
    QStringList m_available;
    quint64 m_searchToken = 0;
    bool m_searchRunning = false;
    FamWindow m_resultsHead;
    /* Set while the opt-in self-check mutation is in the watched folder, so it can be undone. */
    QString m_probe;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */
